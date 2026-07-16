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
