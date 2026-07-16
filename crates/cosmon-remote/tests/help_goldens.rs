// SPDX-License-Identifier: AGPL-3.0-only

//! Golden `--help` snapshots for the delivered tenant binary.
//!
//! The fusion of the tenant CLI is gated on a byte-identical `--help`
//! surface: these goldens are captured on the PRE-fusion binary and the
//! post-fusion binary must reproduce every pre-existing command's help
//! byte-for-byte. A divergence of a single flag is a disguised breaking
//! change — this test catches it and forces a conscious choice.
//!
//! Mechanics: the test spawns the real binary (`CARGO_BIN_EXE`) — the
//! same bytes a tenant runs — and discovers the command tree by parsing
//! the `Commands:` section of each `--help` output recursively. No
//! hand-maintained path list: a new subcommand is discovered (and then
//! fails for lack of a golden until one is blessed consciously).
//!
//! Blessing: `UPDATE_GOLDENS=1 cargo test -p cosmon-remote --test help_goldens`
//! rewrites the files under `tests/goldens/`. Re-blessing an EXISTING
//! golden after the pre-fusion capture is the conscious-choice gesture;
//! it must be argued in the CHANGELOG.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cosmon-remote")
}

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
}

/// Run `<bin> <path...> --help` and return stdout bytes. `--help`
/// always exits 0 in clap; a non-zero exit is a harness bug.
fn help_output(path: &[String]) -> Vec<u8> {
    let out = Command::new(bin())
        .args(path)
        .arg("--help")
        .output()
        .expect("failed to spawn cosmon-remote");
    assert!(
        out.status.success(),
        "`cosmon-remote {} --help` exited non-zero: {}",
        path.join(" "),
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

/// Parse the subcommand names out of a rendered help page: the lines
/// between `Commands:` and the next blank line, first token each. The
/// implicit clap `help` subcommand is skipped (its help is a
/// pass-through, not a surface of its own).
fn subcommands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line.trim_end() == "Commands:" {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.trim().is_empty() {
                break;
            }
            // Continuation lines of a wrapped description are indented
            // beyond the two-space command column; a command line is
            // exactly two spaces then the name.
            if let Some(rest) = line.strip_prefix("  ") {
                if !rest.starts_with(' ') {
                    if let Some(name) = rest.split_whitespace().next() {
                        if name != "help" {
                            names.push(name.to_owned());
                        }
                    }
                }
            }
        }
    }
    names
}

/// Walk the whole command tree breadth-first, returning
/// `path → help bytes` for every node (root included, keyed `""`).
fn collect_tree() -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut queue: Vec<Vec<String>> = vec![vec![]];
    while let Some(path) = queue.pop() {
        let bytes = help_output(&path);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        out.insert(path.join(" "), bytes);
        for sub in subcommands(&text) {
            let mut next = path.clone();
            next.push(sub);
            queue.push(next);
        }
    }
    out
}

fn golden_file_name(path_key: &str) -> String {
    if path_key.is_empty() {
        "root.help.txt".to_owned()
    } else {
        format!("{}.help.txt", path_key.replace(' ', "_"))
    }
}

#[test]
fn help_surface_matches_goldens() {
    let tree = collect_tree();
    let dir = goldens_dir();
    let bless = std::env::var_os("UPDATE_GOLDENS").is_some();

    if bless {
        std::fs::create_dir_all(&dir).expect("create goldens dir");
    }

    let mut failures = Vec::new();
    for (path_key, bytes) in &tree {
        let file = dir.join(golden_file_name(path_key));
        if bless {
            std::fs::write(&file, bytes).expect("write golden");
            continue;
        }
        match std::fs::read(&file) {
            Ok(expected) if expected == *bytes => {}
            Ok(_) => failures.push(format!(
                "help drift for `cosmon-remote {path_key} --help` vs {}",
                file.display()
            )),
            Err(_) => failures.push(format!(
                "no golden for `cosmon-remote {path_key} --help` — expected {}; \
                 bless consciously with UPDATE_GOLDENS=1 and argue it in the CHANGELOG",
                file.display()
            )),
        }
    }

    // Reverse direction: a golden whose command path no longer exists
    // is a REMOVED surface — a breaking change, never silent.
    if !bless {
        for entry in std::fs::read_dir(&dir).expect("goldens dir must exist — bless first") {
            let name = entry.expect("dir entry").file_name();
            let name = name.to_string_lossy();
            let Some(stem) = name.strip_suffix(".help.txt") else {
                continue;
            };
            // Pre-fusion reference snapshots are kept under a distinct
            // suffix and exempt from the live-tree check.
            if stem.ends_with(".pre-fusion") {
                continue;
            }
            let key = if stem == "root" {
                String::new()
            } else {
                stem.replace('_', " ")
            };
            if !tree.contains_key(&key) {
                failures.push(format!(
                    "golden {name} has no live command — a command was removed (breaking)"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "help golden gate failed:\n  {}",
        failures.join("\n  ")
    );
}

/// The committed `man/cosmon-remote.1` must match the man page
/// rendered from the live clap tree by the hidden `__man-page`
/// subcommand (the same `cs __man-page` pattern, transposed). One
/// source: the clap tree
/// already pinned by the help goldens above; the man page is a
/// deterministic projection of it, never a second snapshot to argue
/// about. Any attribute change flows through this byte comparison.
///
/// To refresh after an intentional tree change:
/// `MAN_UPDATE=1 cargo test -p cosmon-remote --test help_goldens man_page_matches_committed`
#[test]
fn man_page_matches_committed() {
    let out = Command::new(bin())
        .arg("__man-page")
        .output()
        .expect("failed to spawn cosmon-remote");
    assert!(
        out.status.success(),
        "`cosmon-remote __man-page` exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let committed_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("man/cosmon-remote.1");

    if std::env::var_os("MAN_UPDATE").is_some() {
        std::fs::create_dir_all(committed_path.parent().expect("man dir")).expect("create man dir");
        std::fs::write(&committed_path, &out.stdout).expect("write man/cosmon-remote.1");
        return;
    }

    let committed = std::fs::read(&committed_path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — bless first with MAN_UPDATE=1",
            committed_path.display()
        )
    });
    assert_eq!(
        out.stdout, committed,
        "man/cosmon-remote.1 is stale — re-run with MAN_UPDATE=1 to refresh."
    );
}
