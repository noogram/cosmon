// SPDX-License-Identifier: AGPL-3.0-only

//! Static guard: no integration test builds its own prerequisites.
//!
//! # The failure this forbids
//!
//! Several test helpers used to resolve a sibling artifact — the `cs` binary
//! for `cosmon-api`, the `trunk_lock_holder` example for `cosmon-filestore` —
//! by checking whether it existed and, if it did not, shelling out to
//! `cargo build`. Under `cargo test --workspace` that fallback is not a
//! convenience but a race:
//!
//! - test *targets* run in parallel, so several binaries can observe the
//!   artifact missing in the same instant and each spawn its own `cargo`;
//! - those nested invocations then serialize on cargo's build lock, which
//!   the outer `cargo test` may still hold — the observable symptom is a
//!   test that never returns, not a test that is slow;
//! - and when a nested build does finish, it is writing the artifact while
//!   a sibling test is already executing it.
//!
//! An in-process `Once` does not fix this: it deduplicates within one test
//! binary and says nothing about the other binaries the runner started
//! alongside it.
//!
//! # What the fix is, and why this file
//!
//! Resolution is now explicit everywhere: an env override that CI and any
//! prebuild step set, else the artifact under our own profile directory,
//! else a clear error naming the build command. A missing prerequisite reads
//! as a missing prerequisite.
//!
//! That is a property of *every* test helper, not of the two that were fixed,
//! and the natural way to reintroduce the bug is to copy an old helper into a
//! new crate. So the rule is a compile-adjacent fact rather than a review
//! habit: this test reddens the moment any source under a `crates/*/tests/`
//! tree spawns cargo again.
//!
//! It is a source scan, in the idiom of `injection_attribution_census.rs`.
//! It reads text, not semantics — but the drift it catches is textual.

use std::fs;
use std::path::{Path, PathBuf};

/// Spellings that spawn a build from inside a test.
///
/// `env!("CARGO")` is the documented way to name the cargo that invoked the
/// build, and `Command::new("cargo")` is the PATH-lookup variant; between
/// them they cover every nested build this workspace has actually grown.
const CARGO_SPAWN_SPELLINGS: &[&str] = &["env!(\"CARGO\")", "Command::new(\"cargo\")"];

#[test]
fn no_integration_test_spawns_a_nested_cargo_build() {
    let mut offenders = Vec::new();

    for tests_dir in test_trees() {
        for file in rust_sources(&tests_dir) {
            let body = fs::read_to_string(&file).expect("read test source");
            for (n, line) in body.lines().enumerate() {
                // Prose may legitimately name the build command it forbids —
                // this file does exactly that — so comments are not code.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if let Some(hit) = CARGO_SPAWN_SPELLINGS.iter().find(|s| line.contains(**s)) {
                    offenders.push(format!(
                        "{}:{}: {} — {}",
                        file.display(),
                        n + 1,
                        hit,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "integration tests must never build their own prerequisites: a nested \
         `cargo build` races the parallel test runner and can deadlock on \
         cargo's build lock. Resolve the artifact explicitly instead — env \
         override, else the profile directory, else a clear error naming the \
         build command (see `crates/cosmon-api/tests/support/prebuilt.rs`), \
         and have CI prebuild it:\n  {}",
        offenders.join("\n  "),
    );
}

/// Every `crates/<member>/tests/` directory in the workspace.
fn test_trees() -> Vec<PathBuf> {
    let crates = workspace_root().join("crates");
    let mut out = Vec::new();
    let entries = fs::read_dir(&crates).expect("read crates/");
    for entry in entries.flatten() {
        let tests = entry.path().join("tests");
        if tests.is_dir() {
            out.push(tests);
        }
    }
    assert!(
        out.len() > 5,
        "expected many crates with a tests/ tree under {}; found {} — the \
         scan is looking in the wrong place and would pass vacuously",
        crates.display(),
        out.len(),
    );
    out.sort();
    out
}

/// `CARGO_MANIFEST_DIR` is `crates/cosmon-cli/`; the workspace root is two
/// levels up.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("workspace root above crates/cosmon-cli")
}

/// Every `.rs` file under `root`, recursively.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
