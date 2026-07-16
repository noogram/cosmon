// SPDX-License-Identifier: AGPL-3.0-only

//! Trust-score parsing for the `cs peek` TRUST column and `v` detail pane.
//!
//! `cs verify <mol-id>` writes a `verify-report.md` to the molecule
//! directory with a table of checks (PASS / FAIL / SKIP) covering artifact
//! hashes, gate replay, and the event-log chain. The operator sees a single
//! lineage-coverage percentage in `cs peek`; a `v` keypress opens the full
//! claim-level breakdown.
//!
//! Coverage is computed as `PASS / (PASS + FAIL)`: SKIP rows carry no
//! evidence either way and must not dilute the score (one number, honest).
//! When no verify-report exists
//! the column renders grey `—` and the `v` pane says "not verified".

use std::path::Path;

/// A single row parsed out of the verify-report.md table — one check
/// performed during `cs verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Claim {
    pub status: ClaimStatus,
    pub category: String,
    pub name: String,
    pub detail: String,
}

/// Terminal verdict for a single claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimStatus {
    Pass,
    Fail,
    Skip,
}

/// Parsed contents of a molecule's `verify-report.md`.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrustReport {
    pub claims: Vec<Claim>,
}

impl TrustReport {
    pub(crate) fn pass(&self) -> usize {
        self.claims
            .iter()
            .filter(|c| c.status == ClaimStatus::Pass)
            .count()
    }

    pub(crate) fn fail(&self) -> usize {
        self.claims
            .iter()
            .filter(|c| c.status == ClaimStatus::Fail)
            .count()
    }

    pub(crate) fn skip(&self) -> usize {
        self.claims
            .iter()
            .filter(|c| c.status == ClaimStatus::Skip)
            .count()
    }

    /// Lineage coverage as a 0..=100 percentage. Returns `None` when no
    /// claim carries evidence either way (all SKIP or empty report) —
    /// rendering as grey `—` is more honest than rounding 0/0 to 100%.
    pub(crate) fn coverage_pct(&self) -> Option<u8> {
        let pass = self.pass();
        let fail = self.fail();
        let total = pass + fail;
        if total == 0 {
            return None;
        }
        // Integer arithmetic — saturates at 100, exact for the ratios the
        // operator cares about (no floating point rounding drift between
        // the column and the detail pane).
        Some(((pass * 100) / total) as u8)
    }
}

/// Read and parse `verify-report.md` from the given molecule directory.
/// Missing file returns `None`; malformed rows are silently skipped so a
/// partial report still yields a meaningful score (watchdog, not validator).
pub(crate) fn load_report(molecule_dir: &Path) -> Option<TrustReport> {
    let path = molecule_dir.join("verify-report.md");
    let text = std::fs::read_to_string(&path).ok()?;
    Some(parse_report(&text))
}

/// Parse the verify-report markdown body into a [`TrustReport`].
pub(crate) fn parse_report(text: &str) -> TrustReport {
    let mut claims = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with('|') {
            continue;
        }
        let cells = split_table_row(t);
        if cells.len() < 6 {
            // Expect: "" status category name detail "" — 6 cells.
            continue;
        }
        let status_cell = cells[1].trim();
        let category_cell = cells[2].trim();
        let name_cell = cells[3].trim();
        let detail_cell = cells[4].trim();
        // Skip header + alignment rows.
        if status_cell.eq_ignore_ascii_case("status") {
            continue;
        }
        if status_cell.starts_with("---") || status_cell.starts_with(':') {
            continue;
        }
        let Some(status) = parse_status(status_cell) else {
            continue;
        };
        claims.push(Claim {
            status,
            category: category_cell.to_owned(),
            name: unquote_backticks(name_cell),
            detail: detail_cell.to_owned(),
        });
    }
    TrustReport { claims }
}

/// Split a markdown table row on unescaped `|` characters. `cs verify`
/// writes `\|` inside the detail cell when the underlying shell command
/// contained a pipe, so a naïve `str::split('|')` would shred those rows.
fn split_table_row(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut escape = false;
    for ch in line.chars() {
        if escape {
            match ch {
                '|' => current.push('|'),
                // Preserve unknown escape sequences verbatim so we don't
                // silently mangle `\\`, `\*`, or other future escapes.
                other => {
                    current.push('\\');
                    current.push(other);
                }
            }
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '|' => {
                cells.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    cells.push(current);
    cells
}

fn parse_status(s: &str) -> Option<ClaimStatus> {
    // Tolerate surrounding ASCII glyphs (✓, ✗, etc.) some future reports
    // may embed before the keyword.
    let upper = s.to_ascii_uppercase();
    if upper.contains("PASS") {
        Some(ClaimStatus::Pass)
    } else if upper.contains("FAIL") {
        Some(ClaimStatus::Fail)
    } else if upper.contains("SKIP") {
        Some(ClaimStatus::Skip)
    } else {
        None
    }
}

fn unquote_backticks(s: &str) -> String {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
        inner.to_owned()
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Verify Report — task-x

**Summary:** 3 PASS, 1 FAIL, 1 SKIP

| Status | Category | Name | Detail |
|--------|----------|------|--------|
| PASS | artifact | `synthesis.md` | blake3 abc… |
| PASS | artifact | `briefing.md` | blake3 def… |
| FAIL | gate | `test (shell)` | exit 1: cargo test |
| SKIP | event-chain | `events.jsonl` | no log |
| PASS | gate | `build (shell)` | exit 0: cargo check |
";

    #[test]
    fn parses_all_claim_rows() {
        let report = parse_report(SAMPLE);
        assert_eq!(report.claims.len(), 5);
        assert_eq!(report.pass(), 3);
        assert_eq!(report.fail(), 1);
        assert_eq!(report.skip(), 1);
    }

    #[test]
    fn coverage_excludes_skips() {
        // 3 PASS / (3 PASS + 1 FAIL) = 75%. SKIP is ignored.
        let report = parse_report(SAMPLE);
        assert_eq!(report.coverage_pct(), Some(75));
    }

    #[test]
    fn coverage_is_none_when_only_skips() {
        let only_skips = "\
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| SKIP | artifact | `a.md` | no manifest |
";
        assert_eq!(parse_report(only_skips).coverage_pct(), None);
    }

    #[test]
    fn coverage_is_none_for_empty_report() {
        assert_eq!(parse_report("").coverage_pct(), None);
    }

    #[test]
    fn coverage_is_100_when_all_pass() {
        let all_pass = "\
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| PASS | artifact | `a.md` | ok |
| PASS | gate | `b (shell)` | exit 0 |
";
        assert_eq!(parse_report(all_pass).coverage_pct(), Some(100));
    }

    #[test]
    fn coverage_is_0_when_all_fail() {
        let all_fail = "\
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| FAIL | gate | `a (shell)` | exit 1 |
";
        assert_eq!(parse_report(all_fail).coverage_pct(), Some(0));
    }

    #[test]
    fn claim_preserves_name_and_detail() {
        let report = parse_report(SAMPLE);
        let fail = report
            .claims
            .iter()
            .find(|c| c.status == ClaimStatus::Fail)
            .unwrap();
        assert_eq!(fail.category, "gate");
        assert_eq!(fail.name, "test (shell)");
        assert!(fail.detail.contains("cargo test"));
    }

    #[test]
    fn ignores_non_table_lines() {
        let noisy = "\
some prose
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| PASS | artifact | `a.md` | ok |
more prose
garbage | | |
";
        let report = parse_report(noisy);
        assert_eq!(report.claims.len(), 1);
    }

    #[test]
    fn unescapes_pipes_in_detail() {
        let escaped = "\
| Status | Category | Name | Detail |
|--------|----------|------|--------|
| PASS | gate | `x` | cmd a \\| b |
";
        let report = parse_report(escaped);
        assert_eq!(report.claims[0].detail, "cmd a | b");
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(load_report(tmp.path()).is_none());
    }

    #[test]
    fn present_file_is_parsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("verify-report.md"), SAMPLE).unwrap();
        let report = load_report(tmp.path()).unwrap();
        assert_eq!(report.coverage_pct(), Some(75));
    }
}
