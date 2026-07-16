// SPDX-License-Identifier: Apache-2.0

//! Compile-time → ADR sync test for the operator-only verb list.
//!
//! The
//! [`cosmon_thin_cli::coverage::OPERATOR_ONLY`] static list must be a
//! mechanical projection of ADR-080 §5.1's *closed list* table. The
//! test parses the ADR markdown, extracts the verb names from the
//! first column of the §5.1 table, normalises them (`cs <name>` →
//! `<name>`, `cs <name> <subverb>` → `<name> <subverb>`,
//! `cs whisper --to-session <sid>` → `whisper`), and asserts the two
//! sides match as sets.
//!
//! Drift in either direction fails CI: a verb added to the ADR
//! without a code update, *or* a verb added to the code without an
//! ADR amendment, both surface as a missing/unexpected entry.

use cosmon_thin_cli::coverage::OPERATOR_ONLY;
use std::collections::BTreeSet;

const ADR_PATH: &str = "../../docs/adr/080-remote-pilot-port-https-oidc.md";
const SECTION_HEADER: &str = "### 5.1 The closed list (V0 / V1)";

fn read_adr() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ADR_PATH);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read ADR-080 at `{}`: {e}", path.display()))
}

/// Extract the verb names (first column) from the §5.1 markdown
/// table. The table is delimited by:
/// - the section header (`### 5.1 The closed list (V0 / V1)`)
/// - the next markdown header line (`## ` or `### ` etc.)
///
/// Verb names are normalised by dropping the `cs ` prefix and any
/// `--flag <arg>` suffix. The leading/trailing pipe and whitespace
/// are stripped.
fn parse_operator_only_verbs(adr: &str) -> Vec<String> {
    let body = adr
        .split_once(SECTION_HEADER)
        .unwrap_or_else(|| {
            panic!("ADR-080 missing `{SECTION_HEADER}` header — did the section move?")
        })
        .1;

    // Stop at the next markdown header (`### 5.2 ...`). Leaves the
    // table body, the column-header row, and its separator row.
    let body = body
        .lines()
        .take_while(|l| !l.starts_with("## ") && !l.starts_with("### "))
        .collect::<Vec<_>>()
        .join("\n");

    let mut verbs = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') || line.is_empty() {
            continue;
        }
        // Skip header (`| Verb | Why ... |`) and separator (`|---|---|`).
        if line.contains("Verb") && line.contains("Why operator-only") {
            continue;
        }
        if line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }
        // Take the first column, strip the leading `|`.
        let cells: Vec<&str> = line.split('|').collect();
        let first = cells.get(1).copied().unwrap_or("").trim();
        // Cells are wrapped in backticks: `` `cs done` ``. Strip them.
        let stripped = first.trim_matches('`').trim();
        if let Some(rest) = stripped.strip_prefix("cs ") {
            // Drop any `--flag <arg>` suffix — we model the verb at
            // the cs level, not the flag-set level.
            let head = rest.split(" --").next().unwrap_or(rest).trim();
            verbs.push(head.to_owned());
        }
    }
    verbs
}

#[test]
fn parses_at_least_one_verb_from_adr() {
    let adr = read_adr();
    let verbs = parse_operator_only_verbs(&adr);
    assert!(
        !verbs.is_empty(),
        "ADR-080 §5.1 parser returned zero verbs — table format probably changed"
    );
}

#[test]
fn operator_only_set_matches_adr_section_5_1() {
    let adr = read_adr();
    let adr_set: BTreeSet<String> = parse_operator_only_verbs(&adr).into_iter().collect();
    let code_set: BTreeSet<String> = OPERATOR_ONLY.iter().map(|e| e.name.to_owned()).collect();

    let in_adr_only: Vec<_> = adr_set.difference(&code_set).cloned().collect();
    let in_code_only: Vec<_> = code_set.difference(&adr_set).cloned().collect();

    assert!(
        in_adr_only.is_empty(),
        "verbs in ADR-080 §5.1 but missing from \
         cosmon_thin_cli::coverage::OPERATOR_ONLY: {in_adr_only:?}\n\
         (add the entry to the OPERATOR_ONLY const, ordered as in the ADR)"
    );
    assert!(
        in_code_only.is_empty(),
        "verbs in cosmon_thin_cli::coverage::OPERATOR_ONLY but absent from \
         ADR-080 §5.1: {in_code_only:?}\n\
         (either drop the entry or amend the ADR with the new row)"
    );
    assert_eq!(
        adr_set, code_set,
        "operator-only set drift: ADR ↔ code disagreement"
    );
}

#[test]
fn every_operator_only_entry_cites_adr_080_5_1() {
    // Defence-in-depth: today every entry cites `ADR-080 §5.1`. If a
    // future row is sourced from a different ADR (e.g. ADR-077 for
    // `cs done`'s deeper rationale), this test will need updating —
    // and that update is intentional (it forces a thoughtful edit
    // rather than a silent drift).
    for entry in OPERATOR_ONLY {
        assert_eq!(
            entry.adr_ref, "ADR-080 §5.1",
            "entry `{}` cites `{}` — adjust this test if the source \
             of truth changed",
            entry.name, entry.adr_ref
        );
    }
}
