// SPDX-License-Identifier: AGPL-3.0-only

//! Scope-guard — partition a merge's changed paths against a declared
//! perimeter (P3 of `task-20260712-3819`).
//!
//! # The pathology this closes
//!
//! A worker briefed to touch a *narrow* set of files (e.g. "docs only") can
//! silently interpret the task as the whole repository. The forcing incident:
//! molecule `task-20260712-c14e` was briefed to strip em-dashes from
//! `docs/book/src/**.md` + `README.md`; the worker instead rewrote **40 Rust
//! files** under `crates/cosmon-cli/src/cmd/`, which would have broken the
//! golden man-page test and silently changed `cs --help` output. The blast
//! radius was caught only by a hand `git status` of the worktree.
//!
//! # The mechanism — a merge-gate perimeter, not a lock
//!
//! A molecule *declares* an allowed change-perimeter (a set of globs). At
//! `cs done` the set of paths the merge would introduce (`<base>...<branch>`)
//! is partitioned against that perimeter; anything **out of scope** is
//! surfaced. Sibling of the `[git_remote_blocklist]` / `[confidential_blocklist]`
//! merge gates, but this one inspects *which files changed*, not their content
//! or the worktree remotes.
//!
//! # Architectural posture — I/O-free core, injected matcher
//!
//! This module is the **pure, dependency-light** half. It knows nothing about
//! git, globset, or the filesystem: [`partition_changed_paths`] takes an
//! injected `is_allowed` predicate (the CLI layer supplies a `globset` matcher
//! and the `git diff --name-only` change list). This mirrors cosmon's
//! "I/O-free domain logic in core; all I/O behind injectable seams" convention
//! and keeps `cosmon-core` free of a `globset` dependency. The glob dialect,
//! the git invocation, and the warn-vs-abort policy all live in
//! `cosmon-cli::cmd::done`.
//!
//! The guard is a **trace, not a lock** (invariants §8b — *propose mechanisms
//! of verification, do not impose them*). It reports the out-of-scope set; the
//! CLI layer decides whether that is advisory (the default) or a hard abort
//! (strict opt-in). An empty perimeter is inert by construction, so every
//! molecule that predates the knob keeps byte-identical `cs done` behaviour.

/// The two-way split of a merge's changed paths against a declared perimeter.
///
/// Order-preserving relative to the input `changed` slice so the operator sees
/// the paths in the order `git diff` reported them. Both vectors are empty for
/// an empty input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopePartition {
    /// Paths that matched the declared perimeter — the sanctioned blast radius.
    pub in_scope: Vec<String>,
    /// Paths that fell **outside** the declared perimeter — the escapees the
    /// gate exists to surface.
    pub out_of_scope: Vec<String>,
}

impl ScopePartition {
    /// `true` when no path escaped the perimeter — the merge stayed inside the
    /// declared blast radius.
    ///
    /// This is the fast verdict the CLI gate reads: clean ⇒ pass silently;
    /// dirty ⇒ warn or abort depending on policy.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.out_of_scope.is_empty()
    }

    /// Count of paths that escaped the perimeter.
    #[must_use]
    pub fn out_of_scope_count(&self) -> usize {
        self.out_of_scope.len()
    }
}

/// Partition `changed` paths into in-scope / out-of-scope using the injected
/// `is_allowed` matcher.
///
/// `is_allowed(path)` is the seam the CLI layer fills with a compiled
/// `globset::GlobSet::is_match`. The function is order-preserving and clones
/// each path into exactly one of the two buckets. A path that matches *any*
/// declared glob is in-scope; a path that matches none is an escapee.
///
/// # Examples
///
/// ```
/// use cosmon_core::scope_guard::partition_changed_paths;
///
/// let changed = vec![
///     "docs/book/src/intro.md".to_owned(),
///     "README.md".to_owned(),
///     "crates/cosmon-cli/src/cmd/tackle.rs".to_owned(),
/// ];
/// // A toy matcher standing in for the globset the CLI supplies.
/// let allowed = |p: &str| p.starts_with("docs/") || p == "README.md";
/// let part = partition_changed_paths(&changed, allowed);
/// assert_eq!(part.in_scope, ["docs/book/src/intro.md", "README.md"]);
/// assert_eq!(part.out_of_scope, ["crates/cosmon-cli/src/cmd/tackle.rs"]);
/// assert!(!part.is_clean());
/// ```
pub fn partition_changed_paths(
    changed: &[String],
    is_allowed: impl Fn(&str) -> bool,
) -> ScopePartition {
    let mut partition = ScopePartition::default();
    for path in changed {
        if is_allowed(path) {
            partition.in_scope.push(path.clone());
        } else {
            partition.out_of_scope.push(path.clone());
        }
    }
    partition
}

/// Parse a declared scope perimeter from its raw string form.
///
/// The perimeter is carried on a molecule as the `scope_allow` variable
/// (`cs nucleate --var scope_allow="docs/book/src/**,README.md"`). Patterns
/// are separated by commas or newlines, individually trimmed; empty fragments
/// are dropped. Returns an empty `Vec` for empty / whitespace-only input,
/// which the CLI treats as "no perimeter declared → guard inert".
///
/// The split is deliberately forgiving (comma **and** newline) so an operator
/// can write the perimeter either inline (`a,b,c`) or as a multi-line
/// heredoc-style `--var` value without a surprise.
///
/// # Examples
///
/// ```
/// use cosmon_core::scope_guard::parse_scope_perimeter;
///
/// assert_eq!(
///     parse_scope_perimeter("docs/book/src/**, README.md"),
///     vec!["docs/book/src/**".to_owned(), "README.md".to_owned()],
/// );
/// assert!(parse_scope_perimeter("   ").is_empty());
/// ```
#[must_use]
pub fn parse_scope_perimeter(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|frag| !frag.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty change list yields a clean, empty partition regardless of the
    /// matcher — the degenerate case the CLI short-circuits on.
    #[test]
    fn empty_changed_is_clean() {
        let part = partition_changed_paths(&[], |_| false);
        assert!(part.is_clean());
        assert_eq!(part.out_of_scope_count(), 0);
        assert!(part.in_scope.is_empty());
    }

    /// Every path allowed ⇒ nothing escapes ⇒ clean.
    #[test]
    fn all_allowed_is_clean() {
        let changed = vec!["a.md".to_owned(), "b.md".to_owned()];
        let part = partition_changed_paths(&changed, |_| true);
        assert!(part.is_clean());
        assert_eq!(part.in_scope, ["a.md", "b.md"]);
        assert!(part.out_of_scope.is_empty());
    }

    /// A single escapee is surfaced and flips the verdict to dirty — the
    /// task-c14e failure mode in miniature (a docs brief that touched a
    /// crate source file).
    #[test]
    fn single_escapee_is_dirty() {
        let changed = vec![
            "docs/intro.md".to_owned(),
            "crates/cosmon-cli/src/cmd/tackle.rs".to_owned(),
        ];
        let part = partition_changed_paths(&changed, |p| p.starts_with("docs/"));
        assert!(!part.is_clean());
        assert_eq!(part.out_of_scope, ["crates/cosmon-cli/src/cmd/tackle.rs"]);
        assert_eq!(part.in_scope, ["docs/intro.md"]);
        assert_eq!(part.out_of_scope_count(), 1);
    }

    /// Order is preserved within each bucket — the operator reads escapees in
    /// git-diff order, not a re-sorted set.
    #[test]
    fn partition_preserves_input_order() {
        let changed = vec![
            "z.rs".to_owned(),
            "a.md".to_owned(),
            "m.rs".to_owned(),
            "b.md".to_owned(),
        ];
        // Match the `.md` files without an extension comparison (clippy's
        // `case_sensitive_file_extension_comparisons`); the real matcher is a
        // globset supplied by the CLI layer, so the predicate here is arbitrary.
        let part = partition_changed_paths(&changed, |p| p.contains(".md"));
        assert_eq!(part.in_scope, ["a.md", "b.md"]);
        assert_eq!(part.out_of_scope, ["z.rs", "m.rs"]);
    }

    #[test]
    fn parse_perimeter_splits_on_comma_and_newline() {
        assert_eq!(
            parse_scope_perimeter("docs/book/src/**, README.md\nCHANGELOG.md"),
            vec![
                "docs/book/src/**".to_owned(),
                "README.md".to_owned(),
                "CHANGELOG.md".to_owned(),
            ],
        );
    }

    #[test]
    fn parse_perimeter_drops_empty_fragments_and_trims() {
        assert_eq!(
            parse_scope_perimeter(" a ,, ,\n  b  ,\n"),
            vec!["a".to_owned(), "b".to_owned()],
        );
    }

    #[test]
    fn parse_perimeter_empty_input_is_empty() {
        assert!(parse_scope_perimeter("").is_empty());
        assert!(parse_scope_perimeter("   \n , \n ").is_empty());
    }
}
