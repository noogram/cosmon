// SPDX-License-Identifier: AGPL-3.0-only

//! Bootstrap-monotonicity guard — no shell-level neurion lookups in the
//! cosmon substrate.
//!
//! Per [architectural-invariants.md §7c][invariants], cosmon must be able to
//! cold-boot on a fresh machine with no neurion service installed. A
//! single `$(neurion …)` shell substitution in a `LaunchAgent` plist or in
//! a template for `~/.config/cosmon/` is enough to dissolve the
//! git-composable wedge: the operator clones cosmon, runs `cs`, and the
//! substrate fails because it cannot resolve a name through a registry
//! that does not exist yet.
//!
//! This test scans the source-of-truth templates in the repo (the files
//! that get copied or materialised into the canonical install targets)
//! and asserts that none of them contain a `$(neurion …)` substring.
//! It also scans the canonical install targets on the local machine —
//! `~/Library/LaunchAgents/com.cosmon.*.plist` and any `*.toml` under
//! `~/.config/cosmon/` — so a developer's own drift is caught locally
//! even before it lands in the repo.
//!
//! Paired with `restart-fidelity-without-neurion`, this forms the
//! two-sided cultural enforcement:
//! one test proves the substrate *runs* without neurion, the other
//! proves the substrate does not *call* neurion at bootstrap time.
//!
//! [invariants]: ../../../docs/architectural-invariants.md

use std::fs;
use std::path::{Path, PathBuf};

/// The forbidden substring. `$(neurion …)` is a shell command
/// substitution that would invoke the neurion binary at cold boot —
/// exactly the bootstrap-regress the invariant forbids.
const FORBIDDEN: &str = "$(neurion ";

/// Repo-rooted directories that ship source-of-truth templates for
/// files that will end up under `~/Library/LaunchAgents/com.cosmon.*`
/// or `~/.config/cosmon/`. Adding a new template directory here is a
/// deliberate surface extension — the test grows by one entry, not by
/// one wildcard.
const TEMPLATE_DIRS: &[(&str, &[&str])] = &[
    // Every plist in scripts/launchd/ whose `Label` begins with
    // com.cosmon. — the installer copies them to
    // ~/Library/LaunchAgents/ verbatim after `__HOME__` substitution.
    ("scripts/launchd", &["plist"]),
    // Scheduler sample TOML — installed to ~/.config/cosmon/patrols.toml
    // by the operator per scripts/install-scheduler.sh.
    ("crates/cosmon-scheduler/tests/fixtures", &["toml"]),
];

/// Walk the repo templates, return the full list of files that would
/// be scanned. Emitted as a Vec so the assertion failure can reference
/// every file explicitly, not just the first match.
fn collect_template_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for (dir, exts) in TEMPLATE_DIRS {
        let abs = root.join(dir);
        collect_with_ext(&abs, exts, &mut files);
    }
    files
}

/// Walk installed directories on the current machine. The test tolerates
/// any of these being absent — a fresh CI image has no `~/.config/cosmon/`,
/// and that is exactly the state we want to assert is supported.
fn collect_installed_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let launchagents = home.join("Library/LaunchAgents");
        if launchagents.is_dir() {
            if let Ok(entries) = fs::read_dir(&launchagents) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if name.starts_with("com.cosmon.")
                        && std::path::Path::new(name)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("plist"))
                    {
                        files.push(path);
                    }
                }
            }
        }
        let config_cosmon = home.join(".config/cosmon");
        if config_cosmon.is_dir() {
            collect_with_ext(&config_cosmon, &["toml", "plist"], &mut files);
        }
    }
    files
}

fn collect_with_ext(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Do not recurse — templates live one level deep. If this
            // invariant changes, tighten the TEMPLATE_DIRS list rather
            // than widening the walker.
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(path);
        }
    }
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points to crates/cosmon-cli — step up twice.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("cargo manifest is expected to be at crates/cosmon-cli")
}

fn assert_no_forbidden(files: &[PathBuf], source: &str) {
    let mut offenders: Vec<(PathBuf, usize, String)> = Vec::new();
    for path in files {
        let Ok(contents) = fs::read_to_string(path) else {
            // Binary or unreadable — skip. Neither plist nor toml is
            // binary, so this path is benign.
            continue;
        };
        for (idx, line) in contents.lines().enumerate() {
            if line.contains(FORBIDDEN) {
                offenders.push((path.clone(), idx + 1, line.to_string()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{source}: bootstrap-monotonicity violation — the following files \
         contain `{FORBIDDEN}…)`, which would call neurion at cold boot. \
         Cosmon must resolve every substrate dependency via the filesystem, \
         not a registry lookup. See docs/architectural-invariants.md §7c.\n\n{}",
        offenders
            .iter()
            .map(|(p, n, l)| format!("  {}:{n}: {}", p.display(), l.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The templates that ship in the repo must never invoke neurion via
/// shell substitution. This is the CI-visible arm of the guard.
#[test]
fn no_neurion_shell_subst_in_repo_templates() {
    let files = collect_template_files();
    assert!(
        !files.is_empty(),
        "template walker found zero files — the TEMPLATE_DIRS table has \
         drifted from the repo layout. Update \
         crates/cosmon-cli/tests/bootstrap_monotonicity.rs."
    );
    assert_no_forbidden(&files, "repo templates");
}

/// The operator's own installed files must also be clean. This arm
/// catches local drift before it lands in a PR. It is a no-op on a
/// fresh CI image where neither directory exists — and that no-op is
/// itself the point: cosmon cold-boots on a bare machine.
#[test]
fn no_neurion_shell_subst_in_installed_cosmon_files() {
    let files = collect_installed_files();
    // An empty result here is expected on CI and on fresh dev laptops.
    // We still run the scan so that any installed files that *do* exist
    // get audited.
    assert_no_forbidden(&files, "installed files");
}
