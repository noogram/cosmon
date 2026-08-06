// SPDX-License-Identifier: AGPL-3.0-only

//! A source lint over `cosmon-cli`: **every production write of `briefing.md`
//! must deliver the committee-posture pointer**, or say in one line why it is
//! exempt.
//!
//! # Why a lint and not another behavioural test
//!
//! The defect this suite exists for was not a wrong behaviour. It was a
//! *missing call site*. `deliver_committee_posture_reference` — then named
//! `reinstate_committee_posture_reference` — ran from exactly one place,
//! `cs evolve`, and both its unit tests passed: they exercised regeneration,
//! which is the property next to the one that mattered. Nothing measured
//! whether a seat carried its adversarial contract *before* the first
//! regeneration, so `AdversarialBriefing::from_durable_injection` returned
//! `injected = false` for every seat on its step 1, and `plan_committee`
//! rejected those seats as `BriefingNotInjected` (committee-20260728-1668, F1).
//!
//! [`committee_seat_dispatch`](../committee_seat_dispatch.rs) now pins the
//! behaviour at each of the three verbs. What no behavioural test can pin is
//! the **fourth** verb — the one somebody adds next year, which writes
//! `briefing.md` and does not know this contract exists. That is a property of
//! the source, so it is checked against the source.
//!
//! # The rule
//!
//! For every `fs::write` in `crates/cosmon-cli/src` whose target is
//! `briefing.md`, outside `#[cfg(test)]` modules, one of:
//!
//! - a `deliver_committee_posture_reference` call follows within
//!   [`DELIVERY_WINDOW`] lines; or
//! - the write carries an inline `committee-posture: exempt — <reason>` marker
//!   on one of the [`MARKER_WINDOW`] lines above it.
//!
//! The waiver is per write and never per file, for the same reason
//! `scripts/publish.sh` waives per line: a file-level exclusion is a blind spot
//! nobody looks at again, while a marker is a sentence someone had to write and
//! a reviewer reads in the diff.

use std::fs;
use std::path::{Path, PathBuf};

/// How many lines after a briefing write may pass before the delivery call.
///
/// Wide enough for the real interleavings — `cs evolve` writes the briefing,
/// then branches on completion, then delivers — and far too narrow for a
/// delivery in some unrelated later function to accidentally satisfy a write.
const DELIVERY_WINDOW: usize = 40;

/// How many lines above a briefing write the exemption marker may sit.
///
/// Small on purpose: the marker must read as a comment *on this write*, not as
/// a paragraph that happens to be somewhere in the vicinity.
const MARKER_WINDOW: usize = 8;

/// The inline waiver, matching the `publish: allow — <reason>` shape the
/// release gate already uses. The em dash is part of it: it forces a reason.
const EXEMPT_MARKER: &str = "committee-posture: exempt —";

/// A production write of `briefing.md`, rendered as `path/to/file.rs:LINE` —
/// the form an editor makes clickable and a compiler prints.
type Write = String;

/// Strip `#[cfg(test)] mod … { … }` blocks, returning the surviving lines as
/// `(1-indexed line number, text)`.
///
/// Relies on the one thing `cargo fmt --all -- --check` guarantees for a
/// top-level module: its `mod` keyword and its closing brace both sit at column
/// zero. A `#[cfg(test)] fn` is deliberately NOT skipped — `cmd/nucleate.rs`
/// has one in the middle of the file, and skipping to the next column-zero `}`
/// from there would blind the lint to everything after it.
fn production_lines(src: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let is_test_mod = lines[i].trim() == "#[cfg(test)]"
            && lines
                .get(i + 1)
                .is_some_and(|next| next.starts_with("mod ") && next.ends_with('{'));
        if is_test_mod {
            // Skip the attribute, the `mod` line, and the body up to and
            // including the closing brace at column zero.
            i += 2;
            while i < lines.len() && lines[i] != "}" {
                i += 1;
            }
            i += 1;
            continue;
        }
        out.push((i + 1, lines[i]));
        i += 1;
    }
    out
}

/// Does this `fs::write(` call target `briefing.md`?
///
/// The path argument is not always on the call's own line — `cs complete`
/// wraps it onto the next one — so the decision reads a small trailing window.
fn targets_briefing(lines: &[(usize, &str)], at: usize) -> bool {
    lines[at..(at + 3).min(lines.len())]
        .iter()
        .any(|(_, text)| text.contains("briefing_path") || text.contains("\"briefing.md\""))
}

/// Every production write of `briefing.md` in `src` that neither delivers the
/// pointer nor carries an exemption marker.
fn unguarded_writes(src: &str, file: &str) -> Vec<Write> {
    let lines = production_lines(src);
    let mut offenders = Vec::new();
    for (idx, (lineno, text)) in lines.iter().enumerate() {
        if !text.contains("fs::write(") || !targets_briefing(&lines, idx) {
            continue;
        }
        let delivers = lines[idx..(idx + DELIVERY_WINDOW).min(lines.len())]
            .iter()
            .any(|(_, t)| t.contains("deliver_committee_posture_reference"));
        let exempt = lines[idx.saturating_sub(MARKER_WINDOW)..=idx]
            .iter()
            .any(|(_, t)| t.contains(EXEMPT_MARKER));
        if !delivers && !exempt {
            offenders.push(format!("{file}:{lineno}"));
        }
    }
    offenders
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `crates/cosmon-cli/src`, resolved from the manifest so the lint runs from
/// any working directory.
fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Collect every production briefing write in the crate, guarded or not.
fn all_briefing_writes() -> Vec<Write> {
    let mut files = Vec::new();
    rust_sources(&src_root(), &mut files);
    let mut found = Vec::new();
    for path in files {
        let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let name = path
            .strip_prefix(src_root())
            .unwrap_or(&path)
            .display()
            .to_string();
        let lines = production_lines(&src);
        for (idx, (lineno, text)) in lines.iter().enumerate() {
            if text.contains("fs::write(") && targets_briefing(&lines, idx) {
                found.push(format!("{name}:{lineno}"));
            }
        }
    }
    found
}

// ── The lint ────────────────────────────────────────────────────────────────

/// **The rule.** No production write of `briefing.md` may leave a committee
/// seat without its pointer, silently.
#[test]
fn every_production_briefing_write_delivers_the_committee_posture_pointer() {
    let mut files = Vec::new();
    rust_sources(&src_root(), &mut files);
    let mut offenders = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let name = path
            .strip_prefix(src_root())
            .unwrap_or(path)
            .display()
            .to_string();
        offenders.extend(unguarded_writes(&src, &name));
    }
    assert!(
        offenders.is_empty(),
        "these writes of briefing.md neither call \
         `deliver_committee_posture_reference` within {DELIVERY_WINDOW} lines nor carry a \
         `{EXEMPT_MARKER} <reason>` marker above them.\n\
         A committee seat whose briefing is rewritten without the pointer fails witness (2) \
         `BriefingNotInjected` and is dropped from its own roster.\n{offenders:#?}"
    );
}

/// **The lint's own falsifier.** A scanner that finds nothing passes the rule
/// above trivially and forever — including on the day someone moves the
/// briefing writes to a helper it cannot see. So pin that it still sees them,
/// and that a write with no guard is actually caught.
#[test]
fn the_lint_sees_the_writes_it_claims_to_govern() {
    let writes = all_briefing_writes();
    assert!(
        writes.len() >= 4,
        "the lint found only {} production write(s) of briefing.md; it governed 5 when written \
         (nucleate, evolve ×2, complete, the delivery function itself, and the fleet-template \
         injector). Either the writes moved somewhere the scanner cannot see, or the scanner \
         broke — both make the rule above vacuous.\n{writes:#?}",
        writes.len()
    );

    // And an unguarded write is caught, so the rule's silence means something.
    let synthetic = "fn rewrite(briefing_path: &Path) {\n    \
                     fs::write(briefing_path, \"clobbered\").unwrap();\n}\n";
    assert_eq!(
        unguarded_writes(synthetic, "synthetic.rs").len(),
        1,
        "a briefing write with neither a delivery call nor a marker must be caught"
    );
}

/// The exemption is a *marker*, not a file-level pass: the same synthetic write
/// is forgiven only once the sentence is above it.
#[test]
fn the_exemption_marker_is_what_forgives_a_write() {
    let marked = "fn rewrite(briefing_path: &Path) {\n    \
                  // committee-posture: exempt — this write IS the delivery.\n    \
                  fs::write(briefing_path, \"clobbered\").unwrap();\n}\n";
    assert!(
        unguarded_writes(marked, "synthetic.rs").is_empty(),
        "an inline marker above the write must forgive it"
    );

    // A marker far away from the write does not reach it.
    let distant = format!(
        "// {EXEMPT_MARKER} a reason attached to nothing.\n{}fn rewrite(briefing_path: &Path) {{\n    \
         fs::write(briefing_path, \"clobbered\").unwrap();\n}}\n",
        "\n".repeat(MARKER_WINDOW + 2)
    );
    assert_eq!(
        unguarded_writes(&distant, "synthetic.rs").len(),
        1,
        "a marker {MARKER_WINDOW}+ lines away is not a comment on this write"
    );
}

/// `#[cfg(test)]` fixtures write `briefing.md` constantly and must not be
/// governed — but a `#[cfg(test)] fn` in the middle of a file must not blind
/// the scanner to the production code that follows it, which is exactly the
/// shape `cmd/nucleate.rs` has.
#[test]
fn test_modules_are_skipped_and_cfg_test_functions_are_not() {
    let with_test_mod = "fn prod(briefing_path: &Path) {\n    \
                         fs::write(briefing_path, \"x\").unwrap();\n}\n\
                         #[cfg(test)]\nmod tests {\n    \
                         fs::write(briefing_path, \"fixture\").unwrap();\n}\n";
    let offenders = unguarded_writes(with_test_mod, "synthetic.rs");
    assert_eq!(
        offenders.len(),
        1,
        "the fixture write inside `mod tests` must be skipped and the production one kept: \
         {offenders:#?}"
    );
    assert!(
        offenders[0].ends_with(":2"),
        "the production write is on line 2, got {}",
        offenders[0]
    );

    let with_cfg_test_fn = "#[cfg(test)]\nfn helper() {}\n\
                            fn prod(briefing_path: &Path) {\n    \
                            fs::write(briefing_path, \"x\").unwrap();\n}\n";
    assert_eq!(
        unguarded_writes(with_cfg_test_fn, "synthetic.rs").len(),
        1,
        "a `#[cfg(test)] fn` is not a module; everything after it is still production code"
    );
}
