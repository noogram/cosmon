// SPDX-License-Identifier: AGPL-3.0-only

//! Real-binary integration for the runtime's `cs`-subprocess path.
//!
//! # What this proves
//!
//! An earlier fix added `current_exe()` binding for the child `cs` path.
//! Its self-hosting ran in a `tempfile::tempdir()` whose
//! `.cosmon/state/` was synthetic JSON — the parse layer was exercised
//! but the **subprocess invocation against a real `cs` binary** was not.
//! This test closes the loop: it spawns the *actual* `target/debug/cs`
//! produced by `cargo build --bin cs` against a real-shaped (if minimal)
//! cosmon project fixture, and asserts:
//!
//! - The runtime's `read_ensemble` Command (program, args, cwd) is
//!   correctly assembled — no empty-arg footgun.
//! - The child exits 0, the JSON parses, the loop drains without writing
//!   a single `ensemble-read-failed` trace line.
//!
//! # When this test is skipped
//!
//! The test resolves the `cs` binary by walking up from
//! `CARGO_MANIFEST_DIR` to find `target/debug/cs`. In rare CI
//! configurations the binary may be unavailable at test time (e.g. a
//! `cargo test -p cosmon-runtime --no-default-features` invocation that
//! bypasses the workspace-wide build). When that happens the test
//! skips with an explicit `eprintln!` rather than failing, so the
//! workspace-wide `cargo test --workspace` still exercises it cleanly
//! while specialised invocations don't false-positive.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use cosmon_runtime::{
    ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
};

/// Walk up from `CARGO_MANIFEST_DIR` looking for `target/debug/cs`.
///
/// Returns `None` if not found — the caller skips the test instead of
/// failing, because some CI permutations build the binary in a sibling
/// `target` (e.g. `cargo test -p cosmon-runtime` without `--workspace`).
fn locate_cs_binary() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir: &Path = &manifest;
    loop {
        for profile in ["debug", "release"] {
            let candidate = dir.join("target").join(profile).join("cs");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        dir = dir.parent()?;
    }
}

/// Seed a minimal-but-real cosmon project fixture under `root`.
///
/// `cs ensemble --json` walks up looking for `.cosmon/config.toml` (the
/// project-root marker, ADR-069). With a `[project]` stanza in place and
/// an empty `.cosmon/state/`, the binary returns a valid empty-fleet
/// JSON — enough to exercise the subprocess + parse codepath end-to-end.
fn seed_project_fixture(root: &Path) {
    let dot_cosmon = root.join(".cosmon");
    std::fs::create_dir_all(dot_cosmon.join("state")).unwrap();
    std::fs::write(
        dot_cosmon.join("config.toml"),
        r#"[project]
project_id = "test-eb67"
"#,
    )
    .unwrap();
}

#[test]
fn real_cs_binary_drains_without_spurious_ensemble_failure() {
    let Some(cs_bin) = locate_cs_binary() else {
        eprintln!(
            "skipping real-cs-binary test: target/debug/cs not found — \
             run `cargo build --bin cs -p cosmon-cli` first or invoke \
             `cargo test --workspace`",
        );
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    seed_project_fixture(&root);

    let mut config = RuntimeLoopConfig::new(&root);
    config.cs_binary = cs_bin;
    config.poll_interval = Duration::from_millis(100);
    // Short deadline — the fleet is empty, so the loop should reach
    // `Drained` on the first tick. The deadline is the safety net only.
    config.max_runtime = Some(Duration::from_secs(10));

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();
    let shutdown = Arc::new(AtomicBool::new(false));

    let summary = runtime
        .run(&shutdown)
        .expect("loop returns cleanly with real cs binary");

    // The fleet is empty → the very first tick should produce
    // `no-pending-no-running` → `Drained`. Any other exit reason
    // (Shutdown, Deadline) means the subprocess invocation regressed
    // — read the trace tail printed by the assert message to debug.
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
    assert!(
        matches!(summary.exit, ExitReason::Drained),
        "expected ExitReason::Drained on an empty fleet, got {:?}\ntrace:\n{trace}",
        summary.exit,
    );

    let lines: Vec<&str> = trace.lines().collect();
    let spurious: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.contains("ensemble-read-failed"))
        .collect();
    assert!(
        spurious.is_empty(),
        "real cs binary produced {} ensemble-read-failed line(s) — the \
         subprocess invocation regressed:\n{}",
        spurious.len(),
        spurious.join("\n"),
    );
}
