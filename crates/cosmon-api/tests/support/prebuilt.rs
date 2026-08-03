// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit resolution of a *prebuilt* sibling binary for integration tests.
//!
//! # Why this module exists
//!
//! `cs` belongs to `cosmon-cli`, a sibling workspace member, so cargo hands
//! this crate no `CARGO_BIN_EXE_cs`. The previous resolver papered over that
//! by shelling out to `cargo build -p cosmon-cli --bin cs` whenever the
//! binary happened to be missing.
//!
//! That fallback is unsound under a parallel runner, and the failure is not
//! hypothetical: `cargo test --workspace` runs every test *target* in
//! parallel, so several test binaries can miss the artifact at the same
//! instant and each spawn its own nested `cargo`. Those nested invocations
//! then serialize on cargo's build lock — which the outer `cargo test` is
//! *already holding* in some layouts — so what a reader sees is not a slow
//! test but a test that never returns. Even when it does return, the
//! artifact it produced is being written while a sibling test is executing
//! it, and the timing of that race is set by the runner's scheduling rather
//! than by anything in the test.
//!
//! So resolution here is explicit and never builds:
//!
//! 1. an env override — `COSMON_TEST_CS_BIN` — which is what CI and any
//!    prebuild step set, and the only way to point at an artifact outside
//!    the profile directory;
//! 2. otherwise the sibling artifact under our own profile directory,
//!    derived from `current_exe()` (the one anchor that stays correct under
//!    an isolated `CARGO_TARGET_DIR`, and that picks up `release` without a
//!    second hard-coded `debug`);
//! 3. otherwise a **clear error** naming the build command and the env var.
//!
//! Case 3 is the whole point. A missing prerequisite must read as a missing
//! prerequisite, not as a test that quietly grows a build step.

#![allow(dead_code)] // Each test target uses a different subset.

use std::path::{Path, PathBuf};

/// Env var that names a prebuilt `cs` binary explicitly.
///
/// Set by CI's prebuild step and honoured ahead of the profile directory,
/// so an operator can test against an artifact built anywhere.
pub const CS_BIN_ENV: &str = "COSMON_TEST_CS_BIN";

/// The directory cargo wrote *this* test binary into: the test executable
/// lives at `<target>/<profile>/deps/<name>-<hash>`, so its grandparent is
/// `<target>/<profile>` — where sibling bin targets land.
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("test binary lives under <target>/<profile>/deps")
        .to_path_buf()
}

/// Resolve the prebuilt `cs` binary, or explain precisely what is missing.
///
/// Never spawns a build. Returns `Err` with an operator-facing message
/// naming both the expected path and the two ways to satisfy it.
pub fn try_cs_bin() -> Result<PathBuf, String> {
    resolve(
        CS_BIN_ENV,
        std::env::var_os(CS_BIN_ENV).map(PathBuf::from),
        &profile_dir().join("cs"),
        "cs",
    )
}

/// [`try_cs_bin`], panicking with the same message.
///
/// Integration tests want the message, not an error type; a panic here is
/// the honest report that a prerequisite was not built.
pub fn cs_bin() -> PathBuf {
    match try_cs_bin() {
        Ok(path) => path,
        Err(msg) => panic!("{msg}"),
    }
}

/// The shared resolution rule, factored out so it can be exercised directly
/// with a synthetic override and candidate path.
///
/// The override wins when present; an override that points at a nonexistent
/// path is an error rather than a silent fallback, because a typo in a CI
/// variable must not degrade into "test something else". It arrives as a
/// parameter rather than being read here so the rule stays exercisable
/// without mutating the process environment — which in a multi-threaded test
/// binary that also spawns subprocesses is not a safe thing to do.
pub fn resolve(
    env_var: &str,
    override_path: Option<PathBuf>,
    candidate: &Path,
    artifact: &str,
) -> Result<PathBuf, String> {
    if let Some(from_env) = override_path {
        if from_env.is_file() {
            return Ok(from_env);
        }
        return Err(format!(
            "{env_var} is set to `{}`, but no file exists there. \
             Point it at a prebuilt `{artifact}` binary, or unset it to use \
             the profile directory.",
            from_env.display(),
        ));
    }
    if candidate.is_file() {
        return Ok(candidate.to_path_buf());
    }
    Err(format!(
        "prebuilt `{artifact}` binary not found at `{}`. \
         Integration tests never build it implicitly — a nested `cargo build` \
         races the parallel test runner. Build it first with \
         `cargo build -p cosmon-cli --bin cs`, run the whole gate \
         (`cargo test --workspace`, which builds sibling bin targets), or set \
         {env_var} to an existing binary.",
        candidate.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing `cs` is a named prerequisite, never an implicit build.
    ///
    /// This is exactly the property the old nested `cargo build` violated.
    /// The message must carry both remedies so a CI log is actionable
    /// without opening this file.
    #[test]
    fn missing_cs_is_a_named_prerequisite_not_a_build() {
        let err = resolve(
            CS_BIN_ENV,
            None,
            Path::new("/nonexistent/cosmon/target/debug/cs"),
            "cs",
        )
        .expect_err("a nonexistent candidate must not resolve");
        assert!(
            err.contains("cargo build -p cosmon-cli --bin cs"),
            "error must name the build command; got: {err}"
        );
        assert!(
            err.contains(CS_BIN_ENV),
            "error must name the override env var; got: {err}"
        );
    }

    /// A dangling override fails loudly instead of falling through to
    /// whatever else happens to sit in the profile directory.
    #[test]
    fn override_pointing_at_nothing_does_not_fall_back() {
        let err = resolve(
            CS_BIN_ENV,
            Some(PathBuf::from("/nonexistent/cosmon/override")),
            Path::new("/nonexistent/cosmon/candidate"),
            "cs",
        )
        .expect_err("a dangling override must not resolve");
        assert!(
            err.contains("/nonexistent/cosmon/override"),
            "error must quote the override path it rejected; got: {err}"
        );
    }

    /// An existing override is honoured ahead of the profile directory.
    #[test]
    fn override_wins_over_the_profile_directory() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let explicit = tmp.path().join("cs");
        std::fs::write(&explicit, b"#!/bin/sh\n").expect("write stub");
        let resolved = resolve(
            CS_BIN_ENV,
            Some(explicit.clone()),
            Path::new("/nonexistent/cosmon/candidate"),
            "cs",
        )
        .expect("an existing override resolves");
        assert_eq!(resolved, explicit);
    }
}
