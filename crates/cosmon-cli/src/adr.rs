// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic ADR-number arithmetic for the merge gate.
//!
//! # Why this module exists
//!
//! Cosmon runs N workers in parallel, each in an **isolated git worktree**
//! branched from the same `main`. When several of those workers each decide
//! to file "the next ADR", they all scan `docs/adr/`, see the same highest
//! number, and pick the same `ADR-NNN`. The collision is invisible inside
//! each worktree — a branch's filesystem (and its gitignored `state.json`)
//! never sees a peer's allocation. It only surfaces at `cs done`, when the
//! branches converge on `main`. Observed 2026-06-05: the RPP and `LLMPort`
//! workers both minted `ADR-117`; the operator hand-renumbered 117 → 118 at
//! merge time.
//!
//! # Why reservation-at-nucleation is structurally impossible
//!
//! The naïve fix — "reserve the number atomically when the molecule is
//! nucleated" — cannot work in cosmon's branch-per-worker model. There is no
//! shared mutable surface at nucleation time: each worker's worktree is a
//! disjoint filesystem until merge. The *only* convergence point is the base
//! branch at the merge gate. This is the same lesson git already teaches —
//! two people can both create `feature.txt` on separate branches with no
//! error; the conflict is a merge-time fact, not a creation-time one.
//!
//! Therefore unique-monotonic ADR numbering must be resolved **at the merge
//! gate**, exactly where `relocate_workspace_artifacts` already rewrites
//! colliding `molecule/<name>` paths to disjoint ones before merging. This
//! module is the pure, I/O-free arithmetic behind that rewrite: parse a
//! number out of a filename, find the next free number, and plan a
//! deterministic renumber of the branch-added ADRs that collide with what
//! the base branch already carries.
//!
//! See ADR-121 (deterministic ADR renumber at the merge gate) and
//! `cs done`'s `renumber_colliding_adrs`.

use std::collections::BTreeSet;

/// Parse the leading `ADR-NNN` number out of a filename or path.
///
/// Accepts either a bare basename (`117-rpp-central-security.md`) or a path
/// (`docs/adr/117-rpp-central-security.md`); only the final path component is
/// inspected. The number is the run of leading ASCII digits before the first
/// `-`. Returns `None` when the component does not begin with digits (e.g.
/// `INDEX.md`, `README.md`).
///
/// ```
/// use cosmon_cli::adr::parse_adr_number;
/// assert_eq!(parse_adr_number("117-rpp.md"), Some(117));
/// assert_eq!(parse_adr_number("docs/adr/032-p-external-witness.md"), Some(32));
/// assert_eq!(parse_adr_number("INDEX.md"), None);
/// ```
#[must_use]
pub fn parse_adr_number(path: &str) -> Option<u32> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let digits: String = base.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// Render an ADR number in the canonical zero-padded width (`117` → `"117"`,
/// `7` → `"007"`).
///
/// Cosmon's ADR corpus is three-digit padded throughout (`001-…` to `120-…`).
/// Numbers ≥ 1000 are rendered without truncation (`1001` → `"1001"`), which
/// keeps the function total even though the corpus will not realistically
/// reach four digits.
///
/// ```
/// use cosmon_cli::adr::format_adr_number;
/// assert_eq!(format_adr_number(7), "007");
/// assert_eq!(format_adr_number(117), "117");
/// ```
#[must_use]
pub fn format_adr_number(n: u32) -> String {
    format!("{n:03}")
}

/// The next free ADR number above every number in `existing`.
///
/// Returns `max(existing) + 1`, or `1` when `existing` is empty. Duplicates
/// in `existing` are harmless (the max is unaffected).
///
/// ```
/// use cosmon_cli::adr::next_free_number;
/// assert_eq!(next_free_number(&[1, 2, 117, 118]), 119);
/// assert_eq!(next_free_number(&[]), 1);
/// ```
#[must_use]
pub fn next_free_number(existing: &[u32]) -> u32 {
    existing.iter().copied().max().map_or(1, |m| m + 1)
}

/// A duplicate-number group: one ADR number used by two or more files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The shared ADR number.
    pub number: u32,
    /// The filenames (or paths) that all claim `number`, in input order.
    pub files: Vec<String>,
}

/// Find every ADR number claimed by two or more of `files`.
///
/// Files whose name does not start with a number are ignored. The result is
/// sorted by number ascending; within a collision the files preserve input
/// order. This is the audit primitive — note that cosmon's history contains
/// *intentional* duplicates (e.g. two `006-…` ADRs from the early days), so a
/// non-empty result is informational, not necessarily an error.
///
/// ```
/// use cosmon_cli::adr::find_collisions;
/// let files = vec![
///     "117-rpp.md".to_string(),
///     "117-llmport.md".to_string(),
///     "118-other.md".to_string(),
/// ];
/// let c = find_collisions(&files);
/// assert_eq!(c.len(), 1);
/// assert_eq!(c[0].number, 117);
/// assert_eq!(c[0].files.len(), 2);
/// ```
#[must_use]
pub fn find_collisions(files: &[String]) -> Vec<Collision> {
    // Preserve input order per number while grouping.
    let mut order: Vec<u32> = Vec::new();
    let mut groups: std::collections::BTreeMap<u32, Vec<String>> =
        std::collections::BTreeMap::new();
    for f in files {
        if let Some(n) = parse_adr_number(f) {
            groups.entry(n).or_default().push(f.clone());
            if !order.contains(&n) {
                order.push(n);
            }
        }
    }
    groups
        .into_iter()
        .filter(|(_, fs)| fs.len() >= 2)
        .map(|(number, files)| Collision { number, files })
        .collect()
}

/// One planned renumber: rename `old_path`'s ADR number to `new_number`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenumberPlan {
    /// The path as the branch added it (e.g. `docs/adr/117-rpp.md`).
    pub old_path: String,
    /// The path after renumbering (e.g. `docs/adr/120-rpp.md`).
    pub new_path: String,
    /// The colliding number the branch tried to claim.
    pub old_number: u32,
    /// The deterministically-assigned free number.
    pub new_number: u32,
}

/// Build the deterministic path after replacing the leading numeric prefix.
///
/// `docs/adr/117-rpp.md` with `new_number = 120` → `docs/adr/120-rpp.md`.
/// The directory portion and the slug are preserved verbatim; only the
/// leading digit run of the final component is swapped, padded to three
/// digits. Returns `None` when the final component has no numeric prefix.
#[must_use]
pub fn renumbered_path(old_path: &str, new_number: u32) -> Option<String> {
    let (dir, base) = match old_path.rfind('/') {
        Some(i) => (&old_path[..=i], &old_path[i + 1..]),
        None => ("", old_path),
    };
    let digit_len = base.chars().take_while(char::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let rest = &base[digit_len..];
    Some(format!("{dir}{}{rest}", format_adr_number(new_number)))
}

/// Plan a deterministic renumber of the ADRs a branch added that collide
/// with the base branch (or with each other).
///
/// `base_numbers` is the set of ADR numbers already present on the merge
/// target. `branch_added` is the list of ADR file paths the worker's branch
/// *added* relative to that base. A branch-added ADR is renumbered when its
/// number is already owned by the base, or when an earlier branch-added ADR
/// already claimed it in this batch; the first un-owned claimant keeps its
/// number. New numbers are assigned strictly above the running maximum, so
/// the output is collision-free and monotonic regardless of input order.
///
/// Processing order is the sorted `branch_added` order, making the plan a
/// pure function of its inputs (two workers landing in either order get the
/// same assignment).
///
/// ```
/// use cosmon_cli::adr::plan_renumber;
/// // Base owns 117; the branch also added a different 117 → renumber it.
/// let base = vec![1, 117];
/// let added = vec!["docs/adr/117-llmport.md".to_string()];
/// let plan = plan_renumber(&base, &added);
/// assert_eq!(plan.len(), 1);
/// assert_eq!(plan[0].new_number, 118);
/// assert_eq!(plan[0].new_path, "docs/adr/118-llmport.md");
/// ```
#[must_use]
pub fn plan_renumber(base_numbers: &[u32], branch_added: &[String]) -> Vec<RenumberPlan> {
    let mut used: BTreeSet<u32> = base_numbers.iter().copied().collect();

    let mut sorted: Vec<&String> = branch_added.iter().collect();
    sorted.sort();

    let mut plans = Vec::new();
    for path in sorted {
        let Some(n) = parse_adr_number(path) else {
            continue;
        };
        if used.contains(&n) {
            // Collision: base (or an earlier branch ADR) already owns `n`.
            let used_vec: Vec<u32> = used.iter().copied().collect();
            let new_number = next_free_number(&used_vec);
            if let Some(new_path) = renumbered_path(path, new_number) {
                plans.push(RenumberPlan {
                    old_path: path.clone(),
                    new_path,
                    old_number: n,
                    new_number,
                });
                used.insert(new_number);
            }
        } else {
            // First un-owned claimant keeps its number.
            used.insert(n);
        }
    }
    plans
}

/// Rewrite an ADR file's own title heading from `ADR-<old>` to `ADR-<new>`.
///
/// Cosmon ADRs open with `# ADR-NNN — Title`. When the file is renumbered,
/// only that *self-reference* in the leading heading is rewritten; citations
/// to *other* ADRs elsewhere in the body are deliberately left untouched
/// (they point at established records, not at this file). The rewrite is
/// conservative on purpose: it replaces `ADR-<old, zero-padded>` only on
/// heading lines that start with `#`, so a body sentence mentioning the old
/// number is not silently mangled.
///
/// Cross-references *to* the renumbered ADR from other files are out of
/// scope — a brand-new ADR at merge time is rarely cited yet, which is
/// exactly the fleet-collision case. See ADR-121 for the documented limit.
///
/// ```
/// use cosmon_cli::adr::rewrite_self_reference;
/// let body = "# ADR-117 — RPP\n\nThis supersedes ADR-113.\n";
/// let out = rewrite_self_reference(body, 117, 120);
/// assert!(out.starts_with("# ADR-120 — RPP"));
/// assert!(out.contains("supersedes ADR-113")); // other refs untouched
/// ```
#[must_use]
pub fn rewrite_self_reference(content: &str, old_number: u32, new_number: u32) -> String {
    let old_tok = format!("ADR-{}", format_adr_number(old_number));
    let new_tok = format!("ADR-{}", format_adr_number(new_number));
    let mut out = String::with_capacity(content.len());
    let mut rewritten = false;
    for line in content.split_inclusive('\n') {
        if !rewritten && line.trim_start().starts_with('#') && line.contains(&old_tok) {
            out.push_str(&line.replacen(&old_tok, &new_tok, 1));
            rewritten = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_leading_number() {
        assert_eq!(parse_adr_number("117-rpp.md"), Some(117));
        assert_eq!(parse_adr_number("001-state.md"), Some(1));
        assert_eq!(parse_adr_number("docs/adr/082-baseline.md"), Some(82));
    }

    #[test]
    fn parse_handles_p_suffix_variant() {
        // `032-p-external-witness-axiom.md` is ADR number 32 (a deliberate
        // historical duplicate). The leading digit run is what counts.
        assert_eq!(
            parse_adr_number("032-p-external-witness-axiom.md"),
            Some(32)
        );
    }

    #[test]
    fn parse_rejects_non_numeric_prefix() {
        assert_eq!(parse_adr_number("INDEX.md"), None);
        assert_eq!(parse_adr_number("README.md"), None);
        assert_eq!(parse_adr_number("docs/adr/README.md"), None);
    }

    #[test]
    fn next_free_is_max_plus_one() {
        assert_eq!(next_free_number(&[1, 2, 117, 118]), 119);
        assert_eq!(next_free_number(&[119, 116, 116]), 120);
        assert_eq!(next_free_number(&[]), 1);
    }

    #[test]
    fn format_pads_to_three_digits() {
        assert_eq!(format_adr_number(7), "007");
        assert_eq!(format_adr_number(42), "042");
        assert_eq!(format_adr_number(120), "120");
        assert_eq!(format_adr_number(1001), "1001");
    }

    #[test]
    fn collisions_group_shared_numbers() {
        let files = vec![
            "117-rpp.md".to_string(),
            "117-llmport.md".to_string(),
            "118-other.md".to_string(),
            "116-a.md".to_string(),
            "116-b.md".to_string(),
        ];
        let c = find_collisions(&files);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].number, 116);
        assert_eq!(c[0].files, vec!["116-a.md", "116-b.md"]);
        assert_eq!(c[1].number, 117);
    }

    #[test]
    fn collisions_empty_when_all_unique() {
        let files = vec!["001-a.md".to_string(), "002-b.md".to_string()];
        assert!(find_collisions(&files).is_empty());
    }

    #[test]
    fn renumbered_path_swaps_prefix_keeps_slug_and_dir() {
        assert_eq!(
            renumbered_path("docs/adr/117-rpp-central.md", 120).as_deref(),
            Some("docs/adr/120-rpp-central.md")
        );
        assert_eq!(
            renumbered_path("007-early.md", 333).as_deref(),
            Some("333-early.md")
        );
        assert_eq!(renumbered_path("INDEX.md", 5), None);
    }

    #[test]
    fn plan_renumbers_branch_added_colliding_with_base() {
        let base = vec![1, 2, 117];
        let added = vec!["docs/adr/117-llmport.md".to_string()];
        let plan = plan_renumber(&base, &added);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].old_number, 117);
        assert_eq!(plan[0].new_number, 118);
        assert_eq!(plan[0].new_path, "docs/adr/118-llmport.md");
    }

    #[test]
    fn plan_keeps_first_branch_claimant_renumbers_rest() {
        // Two branch ADRs both claim 120, base has up to 119. The
        // alphabetically-first keeps 120, the second moves to 121.
        let base = vec![119];
        let added = vec![
            "docs/adr/120-zzz.md".to_string(),
            "docs/adr/120-aaa.md".to_string(),
        ];
        let plan = plan_renumber(&base, &added);
        // Only the second-in-sorted-order is renumbered: "120-aaa" sorts
        // before "120-zzz", so "aaa" keeps 120 and "zzz" becomes 121.
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].old_path, "docs/adr/120-zzz.md");
        assert_eq!(plan[0].new_number, 121);
    }

    #[test]
    fn plan_is_noop_when_no_collision() {
        let base = vec![1, 2, 117, 118, 119];
        let added = vec!["docs/adr/120-fresh.md".to_string()];
        assert!(plan_renumber(&base, &added).is_empty());
    }

    #[test]
    fn plan_is_order_independent() {
        let base = vec![119];
        let a = vec![
            "docs/adr/120-aaa.md".to_string(),
            "docs/adr/120-bbb.md".to_string(),
        ];
        let b = vec![
            "docs/adr/120-bbb.md".to_string(),
            "docs/adr/120-aaa.md".to_string(),
        ];
        // Same plan regardless of the order git happened to list the adds.
        assert_eq!(plan_renumber(&base, &a), plan_renumber(&base, &b));
    }

    #[test]
    fn rewrite_self_reference_rewrites_only_the_title_heading() {
        let body = "# ADR-117 — RPP central security\n\n\
                    **Status:** Proposed\n\n\
                    This supersedes ADR-113 and relates to ADR-117 discussions.\n";
        let out = rewrite_self_reference(body, 117, 120);
        assert!(out.starts_with("# ADR-120 — RPP central security"));
        // The body's later mention of ADR-117 and the ADR-113 ref are intact.
        assert!(out.contains("supersedes ADR-113"));
        assert!(out.contains("relates to ADR-117 discussions"));
    }

    #[test]
    fn rewrite_self_reference_noop_when_token_absent() {
        let body = "# Some title\n\nbody\n";
        assert_eq!(rewrite_self_reference(body, 117, 120), body);
    }
}
