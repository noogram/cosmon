// SPDX-License-Identifier: AGPL-3.0-only

//! Build script — cargo rebuild triggers only.
//!
//! Man-page generation used to live here with a hand-maintained
//! subcommand list that had to be kept in sync with the `Command` enum
//! in `src/main.rs`. The single-source refactor replaced that
//! duplication with a hidden `cs __man-page` subcommand
//! that renders the man page from the live clap tree; the committed
//! `man/cs.1` is now regenerated (and CI-golden-checked) via the
//! `help_goldens` integration test.
//!
//! `build.rs` emits the rebuild triggers cargo needs to invalidate the
//! binary when help-relevant sources change, and stamps the binary with
//! the git commit it was built from (`COSMON_BUILD_SHA`).
//!
//! ## Build-SHA stamp — substrate for deploy verification
//!
//! The deploy-gap class: `cs done` runs the `post_merge` hook (`just
//! install`) to refresh the on-disk binary after a worker branch lands,
//! but the install can silently no-op (wrong cwd, swallowed failure,
//! cargo seeing nothing to rebuild). The code lands on main while the
//! deployed binary lags, so worker-green ≠ operator-green. To make the
//! deploy *verifiable* and not merely *attempted*, every `cs` binary is
//! stamped at build time with the commit SHA it was compiled from. The
//! `cs done` deploy-verification step then runs `cs __build-sha` on the
//! freshly-installed binary and asserts it matches the just-merged HEAD.
//!
//! `rerun-if-changed` on the git HEAD pointer (resolved via `git
//! rev-parse --git-path`, which is correct in both plain checkouts and
//! linked worktrees) forces the stamp to track the real commit even
//! when a merge touches no source file in this crate — otherwise cargo
//! would skip the recompile and the stamp would drift stale, producing
//! a false deploy-gap warning.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/cmd/examples.rs");
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-changed=src/root_help.rs");
    println!("cargo:rerun-if-changed=build.rs");

    emit_build_sha();
}

/// Stamp the binary with the git commit SHA it is being built from.
///
/// Emits `cargo:rustc-env=COSMON_BUILD_SHA=<sha>` so `env!("COSMON_BUILD_SHA")`
/// resolves at compile time. Falls back to `"unknown"` when git is
/// unavailable or the source tree is not a checkout (tarball builds, CI
/// without `.git`) — absence of a stamp is a soft signal, never a build
/// failure.
fn emit_build_sha() {
    let sha = git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=COSMON_BUILD_SHA={sha}");

    // Re-run this script (and therefore recompile, since the rustc-env
    // value changes) whenever HEAD moves. `--git-path` resolves the real
    // location inside the `.git` dir or the worktree's gitdir file.
    for logical in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git_output(&["rev-parse", "--git-path", logical]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

/// Run `git <args>` and return trimmed stdout, or `None` on any failure.
fn git_output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
