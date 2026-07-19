// SPDX-License-Identifier: AGPL-3.0-only

//! Minimal library surface for cosmon-cli — exposes the visual charter
//! (`RowKind`, `temp_token`, `whisper_token`, `classify`) so integration
//! tests can lock the exact rendering of every `(status × heartbeat ×
//! blockers × tags × whisper × ghost × drift)` combination.
//!
//! Also exposes the [`sensorium`] loader so integration tests can pin
//! the byte-identical-when-unchanged silence law on the vital strip
//! (`ADR-NEXT-sensorium-strip`) without invoking the `cs` binary.
//!
//! The binary target (`src/main.rs`) still carries the bulk of the CLI;
//! this lib deliberately exposes only the surfaces external tests
//! depend on. See `tests/peek_snapshot.rs`, `tests/ensemble_snapshot.rs`
//! and `tests/sensorium_strip.rs`.

#![allow(clippy::missing_panics_doc, clippy::missing_errors_doc)]

#[path = "visual.rs"]
pub mod visual;

pub mod sensorium;

pub mod tackle_env;

/// Repo-supplied shell trust gate (B5, RCE-by-clone) — the `direnv allow`
/// of cosmon. Every `sh -c` on a string the repository supplies (formula
/// `command`/`verification` steps, `post_merge`/`pre_done` hooks) is gated on
/// a per-repo, human-granted trust marker recorded outside the repo. See the
/// module docs for the threat model and the staleness contract.
pub mod trust;

pub mod adr;

/// Shell-side seams for the seal-verification contract (ADR-140 D4, N4):
/// a real TLC runner and a filesystem verdict cache. The pure decision logic
/// lives in [`cosmon_core::spore::seal`]; `cs spore run` (N5) wires these in.
pub mod spore_seal;

/// Git commit SHA this binary was built from, stamped by `build.rs`.
///
/// The substrate for deploy verification: `cs done`
/// runs the `post_merge` hook to refresh the deployed binary, then runs
/// `cs __build-sha` on the freshly-installed copy and asserts the value
/// matches the just-merged HEAD. A mismatch means the deploy silently
/// no-op'd — the code landed on main but the binary on disk still lags.
///
/// The value is the full 40-char SHA, or `"unknown"` for builds made
/// outside a git checkout. See `build.rs` for how the stamp is kept in
/// sync with the real commit.
pub const BUILD_SHA: &str = env!("COSMON_BUILD_SHA");

/// Working-tree state at compile time, stamped by `build.rs`.
///
/// `"clean"`, `"dirty"` (uncommitted changes — the SHA alone
/// under-identifies the binary), or `"unknown"` (no git available).
pub const BUILD_DIRTY: &str = env!("COSMON_BUILD_DIRTY");

/// UTC date (`YYYY-MM-DD`) the binary was compiled, stamped by
/// `build.rs`, or `"unknown"` when the `date` command is unavailable.
pub const BUILD_DATE: &str = env!("COSMON_BUILD_DATE");

/// The full version string shown by `cs --version`.
///
/// Exists because `CARGO_PKG_VERSION` alone cannot distinguish two
/// binaries built from different repos at the same crate version — the
/// exact failure that motivated the build-SHA stamp (two galaxies
/// overwriting the same `~/.local/bin/cs`, diagnosable only via the
/// hidden `cs __build-sha`). This surfaces the same identity on the
/// *documented* flag: `cs 0.1.0 (78a09f5c, built 2026-07-18)`.
#[must_use]
pub fn long_version() -> String {
    compose_long_version(env!("CARGO_PKG_VERSION"), BUILD_SHA, BUILD_DIRTY, BUILD_DATE)
}

/// Pure composition of the `--version` string from its stamped parts.
///
/// Split from [`long_version`] so the formatting rules are unit-testable
/// without rebuilding under different git states:
///
/// - known SHA → `<pkg> (<sha8>[-dirty][, built <date>])`
/// - unknown SHA (tarball build, no `.git`) → bare `<pkg>`, never a
///   noisy `(unknown)` suffix
/// - unknown date → the `, built …` segment is simply omitted
#[must_use]
pub fn compose_long_version(pkg: &str, sha: &str, dirty: &str, date: &str) -> String {
    if sha == "unknown" {
        return pkg.to_owned();
    }
    let short: String = sha.chars().take(8).collect();
    let dirty_marker = if dirty == "dirty" { "-dirty" } else { "" };
    let built = if date == "unknown" {
        String::new()
    } else {
        format!(", built {date}")
    };
    format!("{pkg} ({short}{dirty_marker}{built})")
}

#[cfg(test)]
mod version_tests {
    use super::compose_long_version;

    #[test]
    fn clean_build_shows_short_sha_and_date() {
        assert_eq!(
            compose_long_version(
                "0.1.0",
                "78a09f5cdeadbeefdeadbeefdeadbeefdeadbeef",
                "clean",
                "2026-07-18"
            ),
            "0.1.0 (78a09f5c, built 2026-07-18)"
        );
    }

    #[test]
    fn dirty_build_carries_marker() {
        assert_eq!(
            compose_long_version(
                "0.1.0",
                "78a09f5cdeadbeefdeadbeefdeadbeefdeadbeef",
                "dirty",
                "2026-07-18"
            ),
            "0.1.0 (78a09f5c-dirty, built 2026-07-18)"
        );
    }

    #[test]
    fn unknown_sha_falls_back_to_bare_version() {
        // Tarball / no-git builds must not render "(unknown)".
        assert_eq!(
            compose_long_version("0.1.0", "unknown", "unknown", "2026-07-18"),
            "0.1.0"
        );
    }

    #[test]
    fn unknown_date_omits_built_segment() {
        assert_eq!(
            compose_long_version(
                "0.1.0",
                "78a09f5cdeadbeefdeadbeefdeadbeefdeadbeef",
                "clean",
                "unknown"
            ),
            "0.1.0 (78a09f5c)"
        );
    }

    #[test]
    fn short_sha_is_not_padded() {
        // A hand-stamped or truncated SHA shorter than 8 chars passes
        // through untouched instead of panicking on a slice bound.
        assert_eq!(
            compose_long_version("0.1.0", "abc", "clean", "unknown"),
            "0.1.0 (abc)"
        );
    }
}
