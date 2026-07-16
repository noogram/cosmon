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
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/role/verifier_cannot_spawn.rs");
    t.compile_fail("tests/compile_fail/role/verifier_cannot_acquire_trunk.rs");
    t.compile_fail("tests/compile_fail/role/implementer_cannot_land.rs");
    t.compile_fail("tests/compile_fail/role/land_without_lock.rs");
}
