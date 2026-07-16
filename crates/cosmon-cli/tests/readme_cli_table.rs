// SPDX-License-Identifier: AGPL-3.0-only

//! Phantom-verb gate for the README "CLI Reference" table.
//!
//! The README advertises a curated set of flagship `cs <verb>` commands.
//! Historically that table rotted into a lie: it listed `cs spawn`, `cs
//! stop`, `cs mail`, `cs nudge` (none of which exist in the clap tree) and
//! omitted the real flagship verbs (`tackle`, `done`, `peek`, `wait`, …).
//! A reader who copy-pasted from the table got "no such subcommand".
//!
//! This test makes that drift impossible. It reads the live subcommand
//! surface from the binary itself (`cs __help-tree`, the same introspection
//! hook `help_goldens` uses) and asserts:
//!
//! 1. **No phantom verbs.** Every `cs <verb>` named in the README table is a
//!    real top-level subcommand. This is karpathy's phantom-verb gate — the
//!    table can never again advertise a command that does not exist.
//! 2. **No silent omission of flagships.** A hard-coded set of load-bearing
//!    verbs (the pilot cycle + monitoring portal + first-contact surface)
//!    must appear in the table, so a future edit cannot quietly drop them
//!    back out the way the original table dropped `tackle`/`done`/`peek`.
//!
//! The binary is the single source of truth; the README is the projection.

use std::collections::BTreeSet;
use std::process::Command;

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

/// The README lives at the workspace root, two levels up from this crate.
fn readme_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md").to_owned()
}

/// Every top-level command path token from the live clap tree.
///
/// `cs __help-tree` prints one command path per line (e.g. `migrate`,
/// `migrate to`); the first whitespace-delimited token is the top-level
/// verb. Hidden plumbing verbs (`__help-tree`, `__man-page`, …) are emitted
/// too — that is fine, they simply widen the "real" set and never appear in
/// the README, so they cannot cause a false failure.
fn real_top_level_verbs() -> BTreeSet<String> {
    let out = Command::new(cs_bin())
        .arg("__help-tree")
        .output()
        .expect("spawn cs __help-tree");
    assert!(out.status.success(), "cs __help-tree exited non-zero");
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf8");
    stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Extract the `cs <verb>` verbs named in the README "CLI Reference" table.
///
/// We scan only the lines between the `## CLI Reference` heading and the
/// next `## ` heading, and pull the verb out of every leading
/// `` | `cs <verb>` `` table cell.
fn readme_table_verbs(readme: &str) -> Vec<String> {
    let mut in_section = false;
    let mut verbs = Vec::new();
    for line in readme.lines() {
        if line.starts_with("## ") {
            in_section = line.trim() == "## CLI Reference";
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim_start();
        // Table rows look like: | `cs <verb>` | description | physics |
        let Some(rest) = trimmed.strip_prefix("| `cs ") else {
            continue;
        };
        if let Some(verb) = rest.split('`').next() {
            // First token only — guards against any `cs verb --flag` form.
            if let Some(verb) = verb.split_whitespace().next() {
                verbs.push(verb.to_owned());
            }
        }
    }
    verbs
}

#[test]
fn readme_cli_table_has_no_phantom_verbs() {
    let readme = std::fs::read_to_string(readme_path()).expect("read README.md");
    let real = real_top_level_verbs();
    let table_verbs = readme_table_verbs(&readme);

    assert!(
        !table_verbs.is_empty(),
        "no `cs <verb>` rows found under the README '## CLI Reference' heading — \
         did the table format change?"
    );

    let phantoms: Vec<&String> = table_verbs.iter().filter(|v| !real.contains(*v)).collect();
    assert!(
        phantoms.is_empty(),
        "README CLI Reference table advertises verb(s) that do not exist in the \
         clap subcommand tree: {phantoms:?}. Real verbs: {real:?}"
    );
}

#[test]
fn readme_cli_table_lists_flagship_verbs() {
    // Load-bearing verbs that must never be silently dropped from the table.
    // The pilot cycle (nucleate→tackle→wait→done), the monitoring portal
    // (peek), the bootstrap (init), and the first-contact surfaces
    // (demo, doctor) — the exact verbs the original lying table omitted.
    const FLAGSHIP: &[&str] = &[
        "init", "nucleate", "tackle", "evolve", "complete", "wait", "done", "peek", "demo",
        "doctor",
    ];

    let readme = std::fs::read_to_string(readme_path()).expect("read README.md");
    let table_verbs: BTreeSet<String> = readme_table_verbs(&readme).into_iter().collect();

    let missing: Vec<&&str> = FLAGSHIP
        .iter()
        .filter(|v| !table_verbs.contains(**v))
        .collect();
    assert!(
        missing.is_empty(),
        "README CLI Reference table is missing flagship verb(s): {missing:?}. \
         These must appear in the table."
    );
}
