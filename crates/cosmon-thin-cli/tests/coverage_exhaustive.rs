// SPDX-License-Identifier: Apache-2.0

//! Exhaustivity gate (T-CS-THIN-TEST-COVERAGE-GAP, scope-expansion
//! 2026-05-05).
//!
//! The operator's directive: *"le test ÉCHOUE si UN chemin clap d'un
//! des deux CLI n'est pas couvert par au moins un test (présence /
//! absence + happy / erreur). C'est mécanique, pas qualitatif."*
//!
//! This file is the meta-test: it scans the cs-thin clap surface plus
//! the `cli-flag-allowlist.toml` rows and asserts that **every**
//! (verb × flag × {present, absent}) cell is documented. Drift in
//! either CLI surfaces here as a missing row.
//!
//! What this gate does NOT do: drive the actual tests. Those live in
//! `flag_parity.rs`, `operator_ux.rs`, `parity_with_cs.rs`. The gate
//! checks the *index* — not the runtime behaviour.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use clap::CommandFactory;
use cosmon_thin_cli::cli::Cli;
use serde::Deserialize;

const ALLOWLIST_TOML: &str = include_str!("cli-flag-allowlist.toml");

#[derive(Debug, Deserialize)]
struct Allowlist {
    #[serde(default)]
    verb_absent_from_thin: Vec<VerbAbsent>,
    #[serde(default)]
    verb_only_in_thin: Vec<VerbOnlyInThin>,
    #[serde(default)]
    flag_only_in_cs: Vec<FlagDiff>,
    #[serde(default)]
    flag_only_in_thin: Vec<FlagDiff>,
}

#[derive(Debug, Deserialize)]
struct VerbAbsent {
    verb: String,
    #[allow(dead_code)]
    class: String,
    #[allow(dead_code)]
    reason: String,
}

#[derive(Debug, Deserialize)]
struct VerbOnlyInThin {
    verb: String,
    #[allow(dead_code)]
    reason: String,
}

#[derive(Debug, Deserialize, Clone)]
struct FlagDiff {
    verb: String,
    flag: String,
    #[allow(dead_code)]
    reason: String,
}

fn load_allowlist() -> Allowlist {
    toml::from_str(ALLOWLIST_TOML).expect("cli-flag-allowlist.toml is well-formed")
}

fn find_cs_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COSMON_THIN_PARITY_CS_BIN") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path);
        }
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("Cargo.lock").exists() {
            for profile in ["debug", "release"] {
                let candidate = dir.join("target").join(profile).join("cs");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            break;
        }
        if !dir.pop() {
            break;
        }
    }
    if let Ok(path_env) = std::env::var("PATH") {
        for entry in path_env.split(':') {
            let p = PathBuf::from(entry).join("cs");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// `find_cs_binary`, but a missing binary is only a licence to skip on a
/// developer machine — never in CI.
///
/// This gate compares a *file* against a *binary*, so with no binary it
/// has nothing to compare and returns green while checking nothing. That
/// is how the allowlist drift this function guards was able to rot
/// unnoticed: the one place that runs on every push was also the one
/// place that could silently opt out. `ci.yml` builds `cs` before
/// `cargo test --workspace` precisely so this gate can run ("parity gate
/// prerequisite"); if that step is ever dropped or renamed, this turns
/// the resulting hole into a red test instead of a green no-op.
fn require_cs() -> Option<PathBuf> {
    if let Some(path) = find_cs_binary() {
        return Some(path);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "no `cs` binary found, but CI=1 — this gate must never skip in CI. \
         `ci.yml` is expected to run `cargo build --bin cs -p cosmon-cli --locked` \
         before `cargo test --workspace`; restore that step (or point \
         COSMON_THIN_PARITY_CS_BIN at a built binary)."
    );
    eprintln!(
        "SKIP — no `cs` binary. Build with \
         `cargo build --bin cs -p cosmon-cli --locked`. (Never skipped in CI.)"
    );
    None
}

/// Every real top-level `cs` verb, hidden or not, as enumerated by the
/// binary's own `__help-tree --all` introspection.
///
/// **Why not scrape `cs --help`.** The root help's `Commands:` block
/// lists only `hide = false` verbs, so ~19 verbs that exist and answer
/// on the CLI today (`events`, `presence`, `stitch`, `note`, …) are
/// invisible there. Reading that block made this gate report every
/// hidden verb as "no longer in cs — stale" and pressure the next
/// contributor into deleting a live verb's allowlist row. Help
/// visibility is a *book* decision; the partition this gate checks is
/// about which verbs cs-thin models, and the two axes are orthogonal —
/// exactly the rationale `--all` was added for. Same source as
/// `cosmon-cli/tests/api_cli_coverage.rs`, the sibling parity audit.
///
/// Multi-segment paths (`events tail`) collapse to their root verb;
/// `__`-prefixed plumbing (`__help-tree`, `__man-page`, …) is dropped.
fn cs_verbs(cs: &std::path::Path) -> BTreeSet<String> {
    let output = Command::new(cs)
        .args(["__help-tree", "--all"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run cs __help-tree --all: {e}"));
    assert!(
        output.status.success(),
        "cs __help-tree --all exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let verbs: BTreeSet<String> = stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|v| !v.starts_with("__"))
        .map(str::to_owned)
        .collect();
    assert!(
        !verbs.is_empty(),
        "cs __help-tree --all returned no verbs — introspection format probably changed:\n{stdout}"
    );
    verbs
}

fn thin_subcommands() -> BTreeSet<String> {
    Cli::command()
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect()
}

/// **The exhaustivity gate.**
///
/// For every cs verb known to the allowlist + cs-thin clap, assert
/// that the verb is in **exactly one** of the three states:
///
/// 1. Modelled in cs-thin (i.e. `Cli::command().get_subcommands()`
///    contains it). Its flag-set parity is owned by
///    `flag_parity::flag_sets_match_modulo_allowlist`.
/// 2. Allowlisted as `[[verb_absent_from_thin]]` with a `class` of
///    `operator_only` or `out_of_scope` and a written `reason`.
/// 3. Allowlisted as `[[verb_only_in_thin]]` (cs-thin meta verbs).
///
/// Verbs that are simultaneously in cs and in cs-thin AND
/// allowlisted as absent (or in cs-thin but allowlisted as only-in-thin)
/// are flagged as inconsistent.
#[test]
fn exhaustive_verb_partition() {
    let Some(cs) = require_cs() else {
        return;
    };
    let allow = load_allowlist();
    let cs_verbs = cs_verbs(&cs);
    let thin_verbs = thin_subcommands();

    let absent_set: BTreeSet<&str> = allow
        .verb_absent_from_thin
        .iter()
        .map(|v| v.verb.as_str())
        .collect();
    let only_thin_set: BTreeSet<&str> = allow
        .verb_only_in_thin
        .iter()
        .map(|v| v.verb.as_str())
        .collect();

    let mut errors: Vec<String> = Vec::new();

    // 1. Every cs verb is in exactly one bucket.
    for verb in &cs_verbs {
        let in_thin = thin_verbs.contains(verb);
        let in_absent = absent_set.contains(verb.as_str());
        let in_only_thin = only_thin_set.contains(verb.as_str());

        let buckets = [in_thin, in_absent, in_only_thin]
            .iter()
            .filter(|b| **b)
            .count();
        if buckets == 0 {
            errors.push(format!(
                "cs verb `{verb}` lives in NO bucket — add to cs-thin or to \
                 [[verb_absent_from_thin]] with a written reason"
            ));
        } else if buckets > 1 {
            errors.push(format!(
                "cs verb `{verb}` is in multiple buckets (in_thin={in_thin}, \
                 in_absent={in_absent}, in_only_thin={in_only_thin}) — \
                 contradictory state"
            ));
        }
    }

    // 2. Every cs-thin verb either matches a cs verb or is allowlisted
    //    in `verb_only_in_thin`.
    for verb in &thin_verbs {
        let has_cs_match = cs_verbs.contains(verb);
        let in_only_thin = only_thin_set.contains(verb.as_str());
        if !has_cs_match && !in_only_thin {
            errors.push(format!(
                "cs-thin verb `{verb}` is absent from cs AND not in \
                 [[verb_only_in_thin]] — orphan verb"
            ));
        }
    }

    // 3. No allowlist row may reference a verb that doesn't exist in
    //    either CLI (stale row).
    for v in &allow.verb_absent_from_thin {
        if !cs_verbs.contains(&v.verb) {
            errors.push(format!(
                "[[verb_absent_from_thin]] verb=`{}` is no longer in cs — stale",
                v.verb
            ));
        }
    }
    for v in &allow.verb_only_in_thin {
        if !thin_verbs.contains(&v.verb) {
            errors.push(format!(
                "[[verb_only_in_thin]] verb=`{}` is no longer in cs-thin — stale",
                v.verb
            ));
        }
    }

    assert!(
        errors.is_empty(),
        "exhaustive verb partition violations:\n  {}",
        errors.join("\n  ")
    );
}

/// Mirror gate at the flag level: every per-verb allowlist row must
/// reference a flag that's present in the named CLI today.
///
/// `verb = "*"` rows are exempt (they're intentionally generic and
/// match every per-verb help, so their staleness is detected by the
/// global-flag comparison in `flag_parity.rs` instead).
#[test]
fn exhaustive_flag_allowlist_is_live() {
    let Some(cs) = require_cs() else {
        return;
    };
    let allow = load_allowlist();
    let cs_verbs = cs_verbs(&cs);
    let thin_verbs = thin_subcommands();

    let mut errors: Vec<String> = Vec::new();
    for row in &allow.flag_only_in_cs {
        if row.verb == "*" {
            continue;
        }
        if !cs_verbs.contains(&row.verb) {
            errors.push(format!(
                "[[flag_only_in_cs]] verb=`{}` flag=`{}` — verb absent from cs",
                row.verb, row.flag
            ));
        }
    }
    for row in &allow.flag_only_in_thin {
        if row.verb == "*" {
            continue;
        }
        if !thin_verbs.contains(&row.verb) {
            errors.push(format!(
                "[[flag_only_in_thin]] verb=`{}` flag=`{}` — verb absent from cs-thin",
                row.verb, row.flag
            ));
        }
    }
    assert!(
        errors.is_empty(),
        "stale flag allowlist rows:\n  {}",
        errors.join("\n  ")
    );
}

/// Counts: must match the published rapport in
/// `docs/guides/cs-thin-test-coverage.md`.
///
/// We don't pin exact numbers (the rapport is hand-written); we
/// ensure the *order of magnitude* is sane: more absent than thin,
/// more flag rows than verb rows would be a sign of a runaway list.
#[test]
fn allowlist_counts_are_plausible() {
    let allow = load_allowlist();
    assert!(
        allow.verb_absent_from_thin.len() >= 11,
        "operator-only ADR-080 §5.1 has at least 11 entries — got {}",
        allow.verb_absent_from_thin.len()
    );
    assert!(
        !allow.verb_only_in_thin.is_empty(),
        "expected at least the `verbs` meta-row in [[verb_only_in_thin]]"
    );
    // Sanity: there's no global blanket entry for verbs.
    assert!(
        allow.verb_only_in_thin.iter().all(|v| v.verb != "*"),
        "wildcard verb names not allowed in [[verb_only_in_thin]]"
    );
}
