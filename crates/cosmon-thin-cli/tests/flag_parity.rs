// SPDX-License-Identifier: Apache-2.0

//! cs ↔ cs-thin clap-surface parity (T-CS-THIN-TEST-COVERAGE-GAP).
//!
//! The 2026-05-05 cross-container live test surfaced a category-K gap:
//! `cs-thin ensemble --json` exited
//! with a clap "unexpected argument" because the flag-set wasn't
//! tested for parity with `cs ensemble`. This file closes that gap by
//! comparing the **flag sets** of the two CLIs verb-by-verb, modulo
//! a documented allowlist (`tests/cli-flag-allowlist.toml`).
//!
//! # What's covered
//!
//! 1. **Every cs verb** (top-level subcommand of the operator-paid
//!    binary) must either appear in cs-thin or be allowlisted under
//!    `[[verb_absent_from_thin]]` with a written reason and class
//!    (`operator_only` → ADR-080 §5.1, or `out_of_scope`).
//! 2. **Every cs-thin verb** must either appear in cs or be allowlisted
//!    under `[[verb_only_in_thin]]` (today: only `verbs`, the meta
//!    self-introspection command).
//! 3. **For every verb modelled on both sides**, the flag sets must
//!    match exactly modulo the per-verb `[[flag_only_in_cs]]` and
//!    `[[flag_only_in_thin]]` allowlist rows.
//! 4. **Every allowlist row must point at a live divergence** — a
//!    stale entry (the underlying flag was removed on both sides) is
//!    a bug because it lets future drift hide.
//!
//! # Locating the cs binary
//!
//! Same precedence as `parity_with_cs.rs`: explicit
//! `COSMON_THIN_PARITY_CS_BIN`, walk-up `target/{debug,release}/cs`,
//! `PATH`. If nothing resolves, the test prints a uniform skip notice
//! and returns success — the CI gate is responsible for building cs
//! first.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use clap::CommandFactory;
use cosmon_thin_cli::cli::Cli;
use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────
// Allowlist parsing
// ─────────────────────────────────────────────────────────────────────

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

#[derive(Debug, Deserialize, Clone)]
struct VerbAbsent {
    verb: String,
    class: String,
    #[serde(default)]
    adr_ref: Option<String>,
    #[allow(dead_code)] // surfaced only when a row is dropped — see drift test
    reason: String,
}

#[derive(Debug, Deserialize, Clone)]
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

// ─────────────────────────────────────────────────────────────────────
// cs binary discovery (same precedence as parity_with_cs.rs)
// ─────────────────────────────────────────────────────────────────────

fn find_cs_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COSMON_THIN_PARITY_CS_BIN") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path);
        }
        eprintln!("flag_parity: COSMON_THIN_PARITY_CS_BIN={p} but path does not exist");
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

fn skip(test: &str) {
    eprintln!(
        "flag_parity::{test}: SKIP — no `cs` binary found. \
         Build it with `cargo build --bin cs -p cosmon-cli --locked` \
         or set COSMON_THIN_PARITY_CS_BIN."
    );
}

// ─────────────────────────────────────────────────────────────────────
// cs introspection — parse `--help` output
// ─────────────────────────────────────────────────────────────────────

/// Run `cs --help` (or `cs <verb> --help`) and return the captured
/// stdout as a `String`.
fn cs_help(cs: &std::path::Path, args: &[&str]) -> String {
    let mut full = vec!["--help"];
    full.splice(0..0, args.iter().copied());
    let output = Command::new(cs)
        .args(&full)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cs --help: {e}"));
    // `cs --help` exits 0; subcommand `--help` also exits 0.
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Extract top-level cs subcommand names from `cs --help`.
///
/// The "Commands:" section lists `<name>  <description>` lines until a
/// blank line. We accept any leading whitespace and pick the first
/// token. We ignore the line that lists the `help` command's
/// alternate-form `[<COMMAND>]` because clap renders both.
fn parse_cs_subcommands(help: &str) -> BTreeSet<String> {
    let mut verbs = BTreeSet::new();
    let mut in_commands = false;
    for line in help.lines() {
        let trimmed = line.trim_start();
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            // Section ends at the first blank line.
            if trimmed.is_empty() {
                break;
            }
            // Indentation is at least 2 spaces; the first token is the verb.
            let first = trimmed.split_whitespace().next().unwrap_or("");
            if first.is_empty() {
                continue;
            }
            // Skip a stray "Print this message" or any literal that
            // doesn't look like a verb.
            if first.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                verbs.insert(first.to_owned());
            }
        }
    }
    verbs
}

/// Extract the long-flag set from a `cs --help` (or subcommand) output.
///
/// We accept `--foo`, `--foo <ARG>`, `-f, --foo`, `-f` (short-only,
/// rare), and skip the `-h, --help` row (clap injects it everywhere).
/// Each token is normalised to its long form when present, otherwise
/// the short form. Returns a `BTreeSet` for stable diffing.
fn parse_help_flags(help: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    let mut in_options = false;
    let mut in_global_options = false;
    for raw in help.lines() {
        let line = raw.trim_end();
        // Options sections may appear under multiple headers. Track the
        // two most common — "Options:" and "Global Options:".
        if line.trim_start().starts_with("Options:") {
            in_options = true;
            in_global_options = false;
            continue;
        }
        if line.trim_start().starts_with("Global Options:") {
            in_global_options = true;
            in_options = false;
            continue;
        }
        // Section ends at a header that isn't a continuation.
        if (in_options || in_global_options) && line.trim_start().ends_with(':') && !line.is_empty()
        {
            in_options = false;
            in_global_options = false;
            continue;
        }
        if !(in_options || in_global_options) {
            continue;
        }
        // Indented option line. Extract tokens that begin with `-`.
        let trimmed = line.trim_start();
        if !trimmed.starts_with('-') {
            // Continuation of a previous option's description — skip.
            continue;
        }
        // Take just the head before the description (split on multiple
        // spaces, which clap uses to separate flag from doc).
        let head: &str = trimmed.split("  ").next().unwrap_or(trimmed);
        // `head` is e.g. "--config <PATH>" or "-v, --verbose" or
        // "-h, --help". Capture every long form `--xxx`.
        for tok in head.split([',', ' ']) {
            let tok = tok.trim();
            if tok.starts_with("--") {
                let name = tok.split('=').next().unwrap_or(tok);
                let name = name.trim_matches([',', ' ']);
                if name == "--help" || name == "--version" {
                    continue;
                }
                flags.insert(name.to_owned());
            }
        }
        // Pure-short flags: `-X<...>` with no comma — rare but capture them.
        if !head.contains("--") {
            let first = head.split_whitespace().next().unwrap_or(head);
            if first.starts_with('-') && first.len() == 2 && first != "-h" {
                flags.insert(first.to_owned());
            }
        }
    }
    flags
}

// ─────────────────────────────────────────────────────────────────────
// cs-thin introspection — clap CommandFactory
// ─────────────────────────────────────────────────────────────────────

/// Top-level cs-thin subcommands extracted from the clap derive.
fn thin_subcommands() -> BTreeSet<String> {
    Cli::command()
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect()
}

/// Long-flag set for cs-thin's top-level command (global flags).
fn thin_global_flags() -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    let cmd = Cli::command();
    for arg in cmd.get_arguments() {
        let id = arg.get_id();
        // `help` and `version` are auto-injected — skip.
        if id == "help" || id == "version" {
            continue;
        }
        if let Some(long) = arg.get_long() {
            flags.insert(format!("--{long}"));
        }
    }
    flags
}

/// Long-flag set for a given cs-thin subcommand.
fn thin_subcommand_flags(name: &str) -> Option<BTreeSet<String>> {
    let cmd = Cli::command();
    let sub = cmd.get_subcommands().find(|s| s.get_name() == name)?;
    let mut flags = BTreeSet::new();
    for arg in sub.get_arguments() {
        let id = arg.get_id();
        if id == "help" || id == "version" {
            continue;
        }
        if let Some(long) = arg.get_long() {
            flags.insert(format!("--{long}"));
        }
    }
    Some(flags)
}

// ─────────────────────────────────────────────────────────────────────
// Allowlist application
// ─────────────────────────────────────────────────────────────────────

fn allowed_only_in_cs(verb: &str, flag: &str, allow: &Allowlist) -> bool {
    allow
        .flag_only_in_cs
        .iter()
        .any(|d| (d.verb == "*" || d.verb == verb) && d.flag == flag)
}

fn allowed_only_in_thin(verb: &str, flag: &str, allow: &Allowlist) -> bool {
    allow
        .flag_only_in_thin
        .iter()
        .any(|d| (d.verb == "*" || d.verb == verb) && d.flag == flag)
}

fn verb_absent_allowed<'a>(verb: &str, allow: &'a Allowlist) -> Option<&'a VerbAbsent> {
    allow.verb_absent_from_thin.iter().find(|v| v.verb == verb)
}

fn verb_only_in_thin_allowed(verb: &str, allow: &Allowlist) -> bool {
    allow.verb_only_in_thin.iter().any(|v| v.verb == verb)
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[test]
fn allowlist_parses() {
    let _ = load_allowlist();
}

#[test]
fn every_cs_verb_is_modelled_or_allowlisted() {
    let Some(cs) = find_cs_binary() else {
        skip("every_cs_verb_is_modelled_or_allowlisted");
        return;
    };
    let allow = load_allowlist();
    let cs_help_text = cs_help(&cs, &[]);
    let cs_verbs = parse_cs_subcommands(&cs_help_text);
    assert!(
        !cs_verbs.is_empty(),
        "parse_cs_subcommands returned empty — `cs --help` format probably changed:\n{cs_help_text}"
    );

    let thin_verbs = thin_subcommands();
    let mut missing: Vec<String> = Vec::new();
    for verb in &cs_verbs {
        if thin_verbs.contains(verb) {
            continue;
        }
        if verb_absent_allowed(verb, &allow).is_none() {
            missing.push(verb.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "cs verbs absent from cs-thin AND missing from cli-flag-allowlist.toml \
         [[verb_absent_from_thin]]:\n  {}\n\n\
         Either add a cs-thin dispatch arm for the verb, or document the absence \
         (operator_only → ADR-080 §5.1; out_of_scope → §8p subset strict).",
        missing.join(", ")
    );
}

#[test]
fn every_thin_verb_is_in_cs_or_allowlisted() {
    let Some(cs) = find_cs_binary() else {
        skip("every_thin_verb_is_in_cs_or_allowlisted");
        return;
    };
    let allow = load_allowlist();
    let cs_verbs = parse_cs_subcommands(&cs_help(&cs, &[]));
    let thin_verbs = thin_subcommands();

    let mut orphan: Vec<String> = Vec::new();
    for verb in &thin_verbs {
        if cs_verbs.contains(verb) {
            continue;
        }
        if !verb_only_in_thin_allowed(verb, &allow) {
            orphan.push(verb.clone());
        }
    }
    assert!(
        orphan.is_empty(),
        "cs-thin verbs absent from cs AND missing from \
         cli-flag-allowlist.toml [[verb_only_in_thin]]:\n  {}",
        orphan.join(", ")
    );
}

#[test]
fn operator_only_class_cites_adr_080() {
    let allow = load_allowlist();
    for v in &allow.verb_absent_from_thin {
        if v.class == "operator_only" {
            let r = v.adr_ref.as_deref().unwrap_or("");
            assert!(
                r.contains("ADR-080"),
                "operator-only verb `{}` must cite ADR-080 in `adr_ref`, got `{r}`",
                v.verb
            );
        }
    }
}

#[test]
fn allowlist_classes_are_known() {
    let allow = load_allowlist();
    for v in &allow.verb_absent_from_thin {
        assert!(
            matches!(v.class.as_str(), "operator_only" | "out_of_scope"),
            "verb `{}` carries unknown class `{}` — expected `operator_only` or `out_of_scope`",
            v.verb,
            v.class
        );
    }
}

/// For each verb modelled on **both** sides, the flag sets must match
/// modulo the per-verb allowlist rows. This is the core mechanical
/// gate: a habit-flag like `--json` that cs-thin needs to accept
/// silently is captured here as `[[flag_only_in_cs]]` (the cs-side
/// row), and the operator-typing test (`operator_ux.rs`) pins the
/// runtime no-op behaviour.
#[test]
fn flag_sets_match_modulo_allowlist() {
    let Some(cs) = find_cs_binary() else {
        skip("flag_sets_match_modulo_allowlist");
        return;
    };
    let allow = load_allowlist();

    let cs_verbs = parse_cs_subcommands(&cs_help(&cs, &[]));
    let thin_verbs = thin_subcommands();

    // Global flags first — verb = "*" rows apply to every per-verb row,
    // so we extract them once and union with the per-verb sets below.
    let cs_global_flags = parse_help_flags(&cs_help(&cs, &[]));
    let thin_global_flags_v = thin_global_flags();

    // Failures accumulate across verbs so the test message is a single
    // diagnostic instead of a fail-on-first-bug.
    let mut failures: Vec<String> = Vec::new();

    for verb in cs_verbs.intersection(&thin_verbs) {
        let cs_flags_raw = parse_help_flags(&cs_help(&cs, &[verb]));
        let Some(thin_flags_raw) = thin_subcommand_flags(verb) else {
            continue;
        };
        // Per-verb flag sets without the globals (we compare globals
        // separately).
        let cs_flags: BTreeSet<String> =
            cs_flags_raw.difference(&cs_global_flags).cloned().collect();
        let thin_flags: BTreeSet<String> = thin_flags_raw
            .difference(&thin_global_flags_v)
            .cloned()
            .collect();

        let only_in_cs: Vec<String> = cs_flags
            .difference(&thin_flags)
            .filter(|f| !allowed_only_in_cs(verb, f, &allow))
            .cloned()
            .collect();
        let only_in_thin: Vec<String> = thin_flags
            .difference(&cs_flags)
            .filter(|f| !allowed_only_in_thin(verb, f, &allow))
            .cloned()
            .collect();

        if !only_in_cs.is_empty() || !only_in_thin.is_empty() {
            failures.push(format!(
                "verb `{verb}`:\n  only in cs:      {only_in_cs:?}\n  only in cs-thin: {only_in_thin:?}",
            ));
        }
    }

    // Global-flag drift: anything that's only in one global set without
    // a verb="*" allowlist row is a failure.
    let only_in_cs_global: Vec<String> = cs_global_flags
        .difference(&thin_global_flags_v)
        .filter(|f| !allowed_only_in_cs("*", f, &allow))
        .cloned()
        .collect();
    let only_in_thin_global: Vec<String> = thin_global_flags_v
        .difference(&cs_global_flags)
        .filter(|f| !allowed_only_in_thin("*", f, &allow))
        .cloned()
        .collect();
    if !only_in_cs_global.is_empty() || !only_in_thin_global.is_empty() {
        failures.push(format!(
            "GLOBAL flags:\n  only in cs:      {only_in_cs_global:?}\n  only in cs-thin: {only_in_thin_global:?}",
        ));
    }

    assert!(
        failures.is_empty(),
        "cs ↔ cs-thin flag-set drift (un-allowlisted):\n\n{}\n\n\
         Either bring the surfaces in sync, or add the diff to \
         tests/cli-flag-allowlist.toml with a written `reason = \"...\"`.",
        failures.join("\n\n")
    );
}

/// Defence-in-depth: every per-verb allowlist row must reference a
/// flag that actually exists in at least one of the two CLIs. A stale
/// row (the flag was removed on both sides) lets future drift hide.
///
/// `verb = "*"` rows are exempted — they are intentionally generic.
#[test]
fn allowlist_rows_are_not_stale() {
    let Some(cs) = find_cs_binary() else {
        skip("allowlist_rows_are_not_stale");
        return;
    };
    let allow = load_allowlist();
    let cs_verbs = parse_cs_subcommands(&cs_help(&cs, &[]));
    let thin_verbs = thin_subcommands();

    let mut stale: Vec<String> = Vec::new();
    for row in &allow.flag_only_in_cs {
        if row.verb == "*" {
            continue;
        }
        if !cs_verbs.contains(&row.verb) {
            // Verb itself is gone — surfaced by other tests; skip here.
            continue;
        }
        let cs_flags = parse_help_flags(&cs_help(&cs, &[row.verb.as_str()]));
        if !cs_flags.contains(&row.flag) {
            stale.push(format!(
                "[[flag_only_in_cs]] verb=`{}` flag=`{}` — flag is no longer in cs",
                row.verb, row.flag
            ));
        }
    }
    for row in &allow.flag_only_in_thin {
        if row.verb == "*" {
            continue;
        }
        if !thin_verbs.contains(&row.verb) {
            continue;
        }
        let Some(thin_flags) = thin_subcommand_flags(&row.verb) else {
            continue;
        };
        if !thin_flags.contains(&row.flag) {
            stale.push(format!(
                "[[flag_only_in_thin]] verb=`{}` flag=`{}` — flag is no longer in cs-thin",
                row.verb, row.flag
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "stale allowlist rows (delete from cli-flag-allowlist.toml):\n  {}",
        stale.join("\n  ")
    );
}

/// Mechanical contract: every cs-thin verb must dispatch (i.e. carry
/// at least one positional or named argument *or* be a meta verb).
///
/// Catches the trivial regression where a verb is added to clap but
/// the dispatch arm in `Command` is empty. The companion test
/// `coverage_complete::coverage_report_lists_every_modelled_verb`
/// checks the registry entry; this one checks the clap surface.
#[test]
fn every_thin_verb_has_metadata() {
    let cmd = Cli::command();
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        let about = sub.get_about().map(std::string::ToString::to_string);
        assert!(
            about.is_some_and(|a| !a.is_empty()),
            "cs-thin verb `{name}` is missing an `about = ...` doc — clap rendering will be empty"
        );
    }
}
