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

pub mod briefing_receipt_hook;

/// Argv contract of the detached `cs realized-watch` re-exec `cs tackle`
/// arms at dispatch — shared by the spawner and the integration test that
/// exercises the real re-exec, so the two can never drift.
pub mod realized_watcher;

// The briefing-submit receipt kernel, plus the durable record and argv that let
// a detached `cs briefing-backstop` child re-run it after `cs tackle` has
// exited (COSMON #26-B). Documented by the module's own `//!` header and NOT by
// a `///` here: an outer doc comment on the declaration merges with the inner
// one and drags intra-doc link resolution up into *this* module's scope, which
// silently breaks every `[`Item`]` the module writes about itself.
pub mod briefing_backstop;

/// Repo-supplied shell trust gate (B5, RCE-by-clone) — the `direnv allow`
/// of cosmon. Every `sh -c` on a string the repository supplies (formula
/// `command`/`verification` steps, `post_merge`/`pre_done` hooks) is gated on
/// a per-repo, human-granted trust marker recorded outside the repo. See the
/// module docs for the threat model and the staleness contract.
pub mod trust;

pub mod adr;

// The census of everywhere `cs` can put text in a worker's composer
// (COSMON #26 residual). Documented by the module's own `//!` header for the
// same intra-doc-link reason as `briefing_backstop` above.
pub mod injection_provenance;

/// Resolution of a molecule's integration base branch — the single place that
/// answers "which trunk does this molecule's work belong to?" for both the
/// branch cut (`cs tackle`) and the harvest (`cs done`).
pub mod base_branch;

/// Shell-side seams for the seal-verification contract (ADR-140 D4, N4):
/// a real TLC runner and a filesystem verdict cache. The pure decision logic
/// lives in [`cosmon_core::spore::seal`]; `cs spore run` (N5) wires these in.
pub mod spore_seal;

/// Filesystem-aware containment of a germination's per-node output homes
/// (ADR-161). The lexical grammar in [`cosmon_core::spore`] closes the *string*
/// attacks; this module closes the *symlink* one, by creating each home with
/// no-follow semantics and canonicalizing it against the real run home.
pub mod spore_containment;

/// Fetching and copying a shareable spore bundle into the current project
/// (`cs spore install`). The grammar and the refusal rules are pure and live in
/// [`cosmon_core::spore::install`]; this module owns the `git` subprocess, the
/// temporary checkout, and the symlink-refusing tree copy.
pub mod spore_install;

/// Git commit SHA this binary was built from, stamped by `build.rs`.
///
/// Provenance, not verification: the SHA answers *"which commit, in
/// which repository, produced this binary"* — the question an operator
/// asks when two galaxies install the same `cs` name. It deliberately
/// does **not** drive the deploy check, because a commit SHA is a graph
/// coordinate and every history rewrite (rebase, squash, projection to
/// a public trunk) moves it while leaving the compiled source
/// byte-identical. [`BUILD_TREE`] is the invariant of those operations
/// and is what `cs done` compares.
///
/// The value is the full 40-char SHA, or `"unknown"` for builds made
/// outside a git checkout. See `build.rs` for how the stamp is kept in
/// sync with the real commit.
pub const BUILD_SHA: &str = env!("COSMON_BUILD_SHA");

/// Git tree OID (content hash) this binary was built from, stamped by
/// `build.rs`.
///
/// The substrate for deploy verification: `cs done` runs the
/// `post_merge` hook to refresh the deployed binary, then runs
/// `cs __build-tree` on the freshly-installed copy and asserts the value
/// matches the just-merged HEAD's tree. A mismatch means the deploy
/// silently no-op'd — the code landed on the trunk but the binary on
/// disk still lags.
///
/// Trees rather than SHAs because the question is *"does this binary
/// contain this code?"*, which is about content: two commits sharing a
/// tree compile to the same program, and a SHA comparison would report
/// a DEPLOY GAP on a binary that is in fact correct.
///
/// The value is the full 40-char tree OID, or `"unknown"` for builds
/// made outside a git checkout.
pub const BUILD_TREE: &str = env!("COSMON_BUILD_TREE");

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
/// *documented* flag:
/// `cs 0.1.0 (78a09f5c, tree cebf2425, built 2026-07-18)`.
///
/// The tree segment is there because it, and not the SHA, is what the
/// deploy check compares: when `cs done` reports a gap, the operator
/// must be able to read the two operands off `cs --version` and
/// `git rev-parse HEAD^{tree}` without hidden plumbing.
#[must_use]
pub fn long_version() -> String {
    compose_long_version(
        env!("CARGO_PKG_VERSION"),
        BUILD_SHA,
        BUILD_DIRTY,
        BUILD_TREE,
        BUILD_DATE,
    )
}

/// Pure composition of the `--version` string from its stamped parts.
///
/// Split from [`long_version`] so the formatting rules are unit-testable
/// without rebuilding under different git states:
///
/// - known SHA → `<pkg> (<sha8>[-dirty][, tree <tree8>][, built <date>])`
/// - unknown SHA (tarball build, no `.git`) → bare `<pkg>`, never a
///   noisy `(unknown)` suffix
/// - unknown tree → the `, tree …` segment is simply omitted
/// - unknown date → the `, built …` segment is simply omitted
#[must_use]
pub fn compose_long_version(pkg: &str, sha: &str, dirty: &str, tree: &str, date: &str) -> String {
    if sha == "unknown" {
        return pkg.to_owned();
    }
    let short: String = sha.chars().take(8).collect();
    let dirty_marker = if dirty == "dirty" { "-dirty" } else { "" };
    let tree_segment = if tree == "unknown" {
        String::new()
    } else {
        let short_tree: String = tree.chars().take(8).collect();
        format!(", tree {short_tree}")
    };
    let built = if date == "unknown" {
        String::new()
    } else {
        format!(", built {date}")
    };
    format!("{pkg} ({short}{dirty_marker}{tree_segment}{built})")
}

#[cfg(test)]
mod version_tests {
    use super::compose_long_version;

    const SHA: &str = "78a09f5cdeadbeefdeadbeefdeadbeefdeadbeef";
    const TREE: &str = "cebf2425deadbeefdeadbeefdeadbeefdeadbeef";

    #[test]
    fn clean_build_shows_short_sha_tree_and_date() {
        assert_eq!(
            compose_long_version("0.1.0", SHA, "clean", TREE, "2026-07-18"),
            "0.1.0 (78a09f5c, tree cebf2425, built 2026-07-18)"
        );
    }

    #[test]
    fn dirty_build_carries_marker() {
        assert_eq!(
            compose_long_version("0.1.0", SHA, "dirty", TREE, "2026-07-18"),
            "0.1.0 (78a09f5c-dirty, tree cebf2425, built 2026-07-18)"
        );
    }

    #[test]
    fn unknown_sha_falls_back_to_bare_version() {
        // Tarball / no-git builds must not render "(unknown)".
        assert_eq!(
            compose_long_version("0.1.0", "unknown", "unknown", "unknown", "2026-07-18"),
            "0.1.0"
        );
    }

    #[test]
    fn unknown_tree_omits_tree_segment() {
        // A binary stamped before the tree stamp existed, or built with a
        // git that could not resolve `HEAD^{tree}`, renders exactly as it
        // did before rather than a noisy `tree unknown`.
        assert_eq!(
            compose_long_version("0.1.0", SHA, "clean", "unknown", "2026-07-18"),
            "0.1.0 (78a09f5c, built 2026-07-18)"
        );
    }

    #[test]
    fn unknown_date_omits_built_segment() {
        assert_eq!(
            compose_long_version("0.1.0", SHA, "clean", TREE, "unknown"),
            "0.1.0 (78a09f5c, tree cebf2425)"
        );
    }

    #[test]
    fn short_sha_is_not_padded() {
        // A hand-stamped or truncated SHA shorter than 8 chars passes
        // through untouched instead of panicking on a slice bound. Same
        // for the tree OID.
        assert_eq!(
            compose_long_version("0.1.0", "abc", "clean", "de", "unknown"),
            "0.1.0 (abc, tree de)"
        );
    }
}
