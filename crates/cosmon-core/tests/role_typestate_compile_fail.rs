// SPDX-License-Identifier: AGPL-3.0-only

//! Compile-fail typestate tests for [`cosmon_core::role`] (`trybuild`).
//!
//! These tests assert that the *wrong* use of a worker role does not
//! compile — the Phase 3 (von-neumann étage 2) guarantee of ADR-110 I1
//! WRITER-UNIQUE made testable:
//!
//! - `verifier_cannot_spawn` — a [`Verifier`](cosmon_core::role::Verifier)
//!   has no `spawn_child` (it does not implement
//!   [`CanSpawn`](cosmon_core::role::CanSpawn)).
//! - `verifier_cannot_acquire_trunk` — a `Verifier` cannot acquire the
//!   trunk write-token (it does not implement
//!   [`CanWriteTrunk`](cosmon_core::role::CanWriteTrunk)).
//! - `implementer_cannot_land` — an
//!   [`Implementer`](cosmon_core::role::Implementer) cannot `land` (same
//!   missing capability).
//! - `land_without_lock` — even a
//!   [`Stitcher`](cosmon_core::role::Stitcher) cannot `land` while
//!   [`Unlocked`](cosmon_core::role::Unlocked): `land` exists only on the
//!   [`TrunkHeld`](cosmon_core::role::TrunkHeld) state.
//!
//! Run-locally workflow: `TRYBUILD=overwrite cargo test -p cosmon-core
//! --test role_typestate_compile_fail` regenerates the `*.stderr` files;
//! review the diff before committing.

#[test]
fn role_typestate_compile_fail() {
    // trybuild asserts the *exact* rustc diagnostic text against committed
    // `*.stderr` snapshots. Compiler diagnostics are not a stable API: every
    // rustc release rewords notes/spans, so a snapshot generated on one
    // toolchain fails on another. This crate's `rust-toolchain.toml` tracks
    // `channel = "stable"` (latest), so the snapshots would drift red on CI
    // the moment the runner's rustc moves past the one that generated them —
    // gating merges on a compiler-version artifact, not on the typestate
    // property itself.
    //
    // Keep the test as a local/opt-in guard rather than a floating-toolchain
    // CI gate: it runs when `COSMON_TRYBUILD=1` (the same run used to
    // regenerate the snapshots with `TRYBUILD=overwrite`), and is a no-op
    // otherwise. The typestate guarantee is still enforced at every build —
    // the API simply does not expose the forbidden methods — this test only
    // documents the failure messages.
    if std::env::var_os("COSMON_TRYBUILD").is_none() {
        eprintln!(
            "role_typestate_compile_fail: skipped (set COSMON_TRYBUILD=1 to run; \
             trybuild snapshots are rustc-version-specific)"
        );
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/role/verifier_cannot_spawn.rs");
    t.compile_fail("tests/compile_fail/role/verifier_cannot_acquire_trunk.rs");
    t.compile_fail("tests/compile_fail/role/implementer_cannot_land.rs");
    t.compile_fail("tests/compile_fail/role/land_without_lock.rs");
}
