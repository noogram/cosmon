// SPDX-License-Identifier: AGPL-3.0-only

//! Golden-file tests for the CLI documentation surface.
//!
//! The clap derive tree in `src/main.rs` is the single source of truth
//! for every help artifact: `cs --help`, `cs <sub> --help`, the `cs
//! help` grouped reference, and the committed `man/cs.1` man page. The
//! tests here lock that surface against silent drift:
//!
//! 1. **Per-subcommand `--help` goldens.** We walk the full help tree
//!    via the hidden `cs __help-tree` subcommand (which introspects the
//!    real clap tree) and snapshot `cs <path> --help` for each node.
//!    Adding a new subcommand without a matching golden fails the
//!    test; changing a subcommand's `about` / `long_about` without
//!    reviewing the golden fails the test.
//!
//! 2. **Man-page golden.** We generate the man page from the live
//!    clap tree via the hidden `cs __man-page` subcommand and compare
//!    it byte-for-byte to the committed `man/cs.1`. Any attribute
//!    change flows through the test.
//!
//! 3. **Generated Reference golden.** We render the mdBook Reference
//!    pages from the live clap tree via the hidden `cs __markdown-help`
//!    subcommand and compare them byte-for-byte to the committed
//!    `docs/book/src/reference/*.md`. The generated set is the
//!    **filtered** one — only the `command_group_layout()` allowlist,
//!    never the raw internal surface (ADR-B1′ §5.2/§5.5). Any signature
//!    change without regenerating fails the build.
//!
//! 4. **Prose-surface checks** (ADR-B1′ §5.7 / R7). The golden diff only
//!    covers the *generated* pages; two lightweight checks cover the
//!    hand-written and prose surfaces the diff never sees:
//!    - **Command-name grep** — every `cs <verb>` cited anywhere in the
//!      book resolves to a real command (catches drift-D1 phantoms like
//!      `cs recover`).
//!    - **Book-link check** — CI runs `scripts/check-book-links.sh`, which
//!      fails closed on every relative target and heading anchor and reports
//!      external dead/home-redirect links as reviewer warnings.
//!
//! To accept new output: `INSTA_UPDATE=1 cargo test -p cosmon-cli --test help_goldens`
//! then `cargo insta review`. To refresh `man/cs.1` alone: run the
//! regen helper inside the `man_page_matches_committed` test harness
//! (set `MAN_UPDATE=1` and re-run the test). To refresh the generated
//! Reference pages after an intentional CLI change: `REFERENCE_UPDATE=1
//! cargo test -p cosmon-cli --test help_goldens markdown_reference`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cs_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cs")
}

/// Run `cs <args...>` and return (stdout, exit-code).
fn run_cs(args: &[&str]) -> (String, i32) {
    let out = Command::new(cs_bin())
        .args(args)
        .output()
        .expect("spawn cs");
    let stdout = String::from_utf8(out.stdout).expect("stdout is utf8");
    let code = out.status.code().unwrap_or(-1);
    (stdout, code)
}

fn help_paths() -> Vec<Vec<String>> {
    let (stdout, code) = run_cs(&["__help-tree"]);
    assert_eq!(code, 0, "cs __help-tree exited non-zero");
    let mut paths: Vec<Vec<String>> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
        .collect();
    // Prepend the root so `cs --help` is also snapshotted.
    paths.insert(0, Vec::new());
    paths
}

/// Snapshot `cs <path> --help` for every subcommand.
///
/// A single test with per-path snapshots keeps the integration cost
/// low (one binary spawn per path, one insta settings block). The
/// snapshot filename is derived from the path so new subcommands
/// produce visibly-named golden files in `tests/snapshots/`.
#[test]
fn per_subcommand_help_snapshots() {
    let paths = help_paths();
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();

    for path in &paths {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let (stdout, code) = run_cs(&args);
        assert_eq!(
            code,
            0,
            "cs {} --help exited non-zero:\n{}",
            path.join(" "),
            stdout
        );

        let slug = if path.is_empty() {
            "root".to_owned()
        } else {
            path.join("_").replace('-', "_")
        };
        let snapshot_name = format!("help__{slug}");
        insta::assert_snapshot!(snapshot_name, stdout);
    }
}

/// The `cs help` grouped reference is a second surface onto the same
/// tree and gets its own snapshot.
#[test]
fn cs_help_grouped_reference_snapshot() {
    let (stdout, code) = run_cs(&["help"]);
    assert_eq!(code, 0);
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();
    insta::assert_snapshot!("cs_help_grouped_reference", stdout);
}

/// The committed `man/cs.1` must match the man page rendered from the
/// live clap tree. Any attribute change in `src/main.rs` or
/// `root_help.rs` flows through this golden.
///
/// To refresh `man/cs.1` after an intentional change, re-run the test
/// with `MAN_UPDATE=1 cargo test -p cosmon-cli --test help_goldens
/// man_page_matches_committed`.
#[test]
fn man_page_matches_committed() {
    let (stdout, code) = run_cs(&["__man-page"]);
    assert_eq!(code, 0, "cs __man-page exited non-zero");

    let committed_path = concat!(env!("CARGO_MANIFEST_DIR"), "/man/cs.1");

    if std::env::var("MAN_UPDATE").is_ok() {
        std::fs::write(committed_path, stdout.as_bytes()).expect("write man/cs.1");
        return;
    }

    let committed = std::fs::read_to_string(committed_path).expect("read man/cs.1");
    assert_eq!(
        stdout, committed,
        "man/cs.1 is stale — re-run with MAN_UPDATE=1 to refresh."
    );
}

/// The hidden `__help-tree` subcommand itself gets a golden so any
/// accidental addition or removal of a subcommand fails CI.
#[test]
fn help_tree_snapshot() {
    let (stdout, code) = run_cs(&["__help-tree"]);
    assert_eq!(code, 0);
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();
    insta::assert_snapshot!("cs_help_tree", stdout);
}

/// Repo-root `docs/book/src` directory (manifest is `crates/cosmon-cli`).
fn book_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/book/src")
        .canonicalize()
        .expect("resolve docs/book/src")
}

/// The committed `docs/book/src/reference` directory.
fn reference_dir() -> PathBuf {
    book_src_dir().join("reference")
}

/// The generated Reference pages (`cs __markdown-help`) must match the
/// committed `docs/book/src/reference/*.md` byte-for-byte. This is the
/// anti-drift spine of ADR-B1′ §5.5: the Reference is a CI-enforced
/// projection of the clap **signature** surface — add or change a
/// command's signature without regenerating and the build fails.
///
/// The generator emits only the `command_group_layout()` allowlist (the
/// **filtered** set); hidden verbs never appear. The hand-written pages
/// (`exit-codes.md`, `formulas.md`) are intentionally NOT diffed here —
/// they are covered by the prose-surface checks below.
///
/// To refresh after an intentional CLI change:
/// `REFERENCE_UPDATE=1 cargo test -p cosmon-cli --test help_goldens markdown_reference`.
#[test]
fn markdown_reference_matches_committed() {
    let update = std::env::var("REFERENCE_UPDATE").is_ok();
    // Keep the guard alive for the whole test so the temp dir is not
    // reaped before we diff against it.
    let tmp = (!update).then(|| tempfile::tempdir().expect("tempdir"));
    let out_dir = if update {
        reference_dir()
    } else {
        tmp.as_ref().expect("tempdir guard").path().to_path_buf()
    };
    let out_str = out_dir.to_str().expect("utf8 out dir");

    let (_stdout, code) = run_cs(&["__markdown-help", "--out", out_str]);
    assert_eq!(code, 0, "cs __markdown-help exited non-zero");

    if update {
        return;
    }

    // The generated set: overview + one page per group.
    let generated: Vec<String> = std::fs::read_dir(&out_dir)
        .expect("read generated dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    assert!(
        generated.len() >= 8,
        "expected at least 8 generated pages, got {}: {generated:?}",
        generated.len()
    );

    for name in &generated {
        let got = std::fs::read_to_string(out_dir.join(name)).expect("read generated page");
        let committed_path = reference_dir().join(name);
        let committed = std::fs::read_to_string(&committed_path).unwrap_or_else(|_| {
            panic!(
                "committed reference page missing: {} — run REFERENCE_UPDATE=1",
                committed_path.display()
            )
        });
        assert_eq!(
            got, committed,
            "{name} is stale — re-run with REFERENCE_UPDATE=1 to refresh."
        );
    }
}

/// Recursively collect every `*.md` file under `docs/book/src`.
fn book_markdown_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read book dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&book_src_dir(), &mut out);
    out
}

/// Verbs that are cited in the book as **documented forward-references**
/// to commands that do not yet exist (each is flagged in-prose as
/// deferred / "when available" / "SEE ALSO"). They are allowlisted so the
/// phantom check does not flag an intentional aspirational reference. If
/// one of these lands as a real command, drop it from the list.
const FORWARD_REFERENCE_VERBS: &[&str] = &["deploy", "git", "rotate-key"];

/// Every `cs <verb>` cited anywhere in the book must resolve to a real
/// command (ADR-B1′ §5.7 / R7). This catches drift-D1 phantoms — a doc
/// that tells the reader to run a command the binary does not have (e.g.
/// the retired `cs recover`).
///
/// A verb is accepted if it is a non-hidden verb (`cs __help-tree`), OR a
/// real-but-hidden verb (`cs <verb> --help` exits 0 — hidden verbs still
/// parse and run), OR a documented forward-reference. Anything else is a
/// phantom.
#[test]
fn book_command_names_resolve() {
    let non_hidden: BTreeSet<String> = {
        let (stdout, code) = run_cs(&["__help-tree"]);
        assert_eq!(code, 0);
        stdout
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
            .collect()
    };

    // Extract `cs <verb>` tokens (verb = lowercase, may contain `-`).
    let mut cited: BTreeSet<String> = BTreeSet::new();
    for file in book_markdown_files() {
        let text = std::fs::read_to_string(&file).expect("read md");
        for (idx, _) in text.match_indices("cs ") {
            let rest = &text[idx + 3..];
            let verb: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            // Require a sensible verb length and a word boundary before `cs`.
            let boundary_ok = idx == 0
                || text[..idx]
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_ascii_alphanumeric());
            // A verb never starts with `-`. Without this, the `-` accepted
            // above (for `rotate-key`) also swallows flags, so any prose
            // containing `cs --<flag>` — including `cargo build --bin cs
            // --target <triple>`, which cites cargo's flag, not a cs verb —
            // is reported as a phantom command. The check is for drift in
            // what the book tells a reader to *run*; a flag is not a verb.
            let is_verb_shaped = verb.starts_with(|c: char| c.is_ascii_lowercase());
            if boundary_ok && is_verb_shaped && verb.len() >= 3 {
                cited.insert(verb);
            }
        }
    }

    let mut phantoms: Vec<String> = Vec::new();
    for verb in &cited {
        if non_hidden.contains(verb) || FORWARD_REFERENCE_VERBS.contains(&verb.as_str()) {
            continue;
        }
        // Real-but-hidden verbs still parse: `cs <verb> --help` exits 0.
        let (_out, code) = run_cs(&[verb, "--help"]);
        if code != 0 {
            phantoms.push(verb.clone());
        }
    }

    assert!(
        phantoms.is_empty(),
        "book cites `cs <verb>` for non-existent command(s): {phantoms:?} — \
fix the doc or add to FORWARD_REFERENCE_VERBS if intentionally aspirational"
    );
}

/// Every relative `.md` link in the book must resolve to a file on disk
/// (ADR-B1′ §5.7 / R7). This is a self-contained stand-in for
/// `mdbook-linkcheck` (not vendored): it catches a banner or cross-link
/// that points at a page that was renamed, moved, or never written.
#[test]
fn book_internal_links_resolve() {
    let mut broken: Vec<String> = Vec::new();
    for file in book_markdown_files() {
        let text = std::fs::read_to_string(&file).expect("read md");
        let dir = file.parent().expect("md has parent");
        // Scan for `](` … `)` link targets.
        for (idx, _) in text.match_indices("](") {
            let rest = &text[idx + 2..];
            let Some(end) = rest.find(')') else { continue };
            let target = &rest[..end];
            // Only check relative markdown links; skip URLs and anchors.
            if target.starts_with("http") || target.starts_with('#') || target.starts_with('/') {
                continue;
            }
            let path_part = target.split('#').next().unwrap_or(target);
            if !path_part.ends_with(".md") {
                continue;
            }
            let resolved = dir.join(path_part);
            if !resolved.exists() {
                broken.push(format!(
                    "{} -> {target}",
                    file.strip_prefix(book_src_dir()).unwrap_or(&file).display()
                ));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "book has unresolved relative .md link(s):\n  {}",
        broken.join("\n  ")
    );
}
