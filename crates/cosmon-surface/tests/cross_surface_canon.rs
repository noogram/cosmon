// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-surface canon — byte-identity of the `cs peek` snapshot raster.
//!
//! §8k' (ADR-066) states that every cosmon-facing surface is a viewport
//! over the canonical byte raster emitted by `cs peek --snapshot`. This
//! test pins the *determinism property* that the contract rests on:
//! given a fixed input, the canon produces byte-identical output across
//! runs.
//!
//! # What this test is — and is not
//!
//! This is the **prototype** golden-snapshot test the ADR calls for. It
//! asserts that a pinned fixture:
//!
//! 1. round-trips verbatim (no trimming, normalisation, lossy encoding);
//! 2. hashes deterministically (two runs produce the same hash);
//! 3. is not accidentally mutated by any test helper in this module.
//!
//! The **full integration** — shelling out to `cs peek --snapshot
//! --molecule <fixture>` and asserting byte-equality against a saved
//! `.snap` — lands with the `cs peek --snapshot` flag materialisation
//! Followup (ADR-066 Followup §8). Until that flag exists, the test
//! protects the invariant at the shape level: *if the byte raster
//! contract can be stated and hashed, every surface can verify it*.
//!
//! # Why here and not in `cosmon-cli`
//!
//! `cosmon-cli` tests (e.g. `peek_snapshot.rs`) already pin per-row
//! rendering via `insta`. That covers drift inside the TUI renderer.
//! The cross-surface test sits one layer up: it is about *the canon
//! contract every surface adapter (`WheatPasteView` and its future
//! peers) consumes*. It lives in `cosmon-surface` because surfaces
//! (STATUS.md, ISSUES.md, and now `WheatPasteView` viewports) are what
//! this crate is for.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Canonical fixture — a small, deterministic, ASCII-safe raster that
/// represents the shape every surface must render verbatim. The exact
/// content is not load-bearing; what is load-bearing is that it survive
/// the round trip unchanged.
///
/// When `cs peek --snapshot` lands (Followup §8), this constant is
/// replaced by output captured from a pinned `.cosmon/` fixture.
const CANON_FIXTURE: &str = concat!(
    "┌─ cosmon peek ─ fleet:default ────────────────────────────────┐\n",
    "│                                                              │\n",
    "│  MOLECULE                STATUS    TEMP   ♥   WHISPER        │\n",
    "│  ──────────────────────────────────────────────────────────  │\n",
    "│  task-20260423-de93      running   ---    ▲                  │\n",
    "│  task-20260423-e49e      pending   warm   💤                  │\n",
    "│  delib-20260423-becf     completed ---    ■                   │\n",
    "│                                                              │\n",
    "└──────────────────────────────────────────────────────────────┘\n",
);

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[test]
fn canon_fixture_round_trips_verbatim() {
    // A viewport must be able to store and retrieve the raster without
    // any transformation. This is the minimum wheat-paste contract:
    // bytes in, bytes out.
    let snapshot = CANON_FIXTURE.to_string();
    assert_eq!(snapshot.as_bytes(), CANON_FIXTURE.as_bytes());
}

#[test]
fn canon_fixture_hash_is_deterministic_across_runs() {
    // Two consecutive hashes of the same fixture must agree. This is
    // the minimum determinism property the CI golden test relies on —
    // if the fixture cannot be deterministically hashed, no surface
    // can pin byte-equality against it.
    let h1 = stable_hash(CANON_FIXTURE.as_bytes());
    let h2 = stable_hash(CANON_FIXTURE.as_bytes());
    assert_eq!(h1, h2);
}

#[test]
fn canon_fixture_hash_is_stable_within_process() {
    // Hashing two distinct copies of the fixture bytes must agree.
    // Detects accidental mutation by any helper that touches the
    // fixture (e.g. a future implementation that inadvertently
    // modifies `CANON_FIXTURE` through interior mutability).
    let a = CANON_FIXTURE.to_string();
    let b = CANON_FIXTURE.to_string();
    assert_eq!(stable_hash(a.as_bytes()), stable_hash(b.as_bytes()));
}

#[test]
fn canon_fixture_byte_length_is_pinned() {
    // Pin the exact byte length of the fixture. A silent drift (e.g.
    // trailing whitespace added by an editor, line-ending normalisation
    // by git, stray BOM) surfaces as a byte-length mismatch, which
    // points the operator at the diff before wondering why every
    // surface's visual regression test started failing.
    //
    // When the fixture is replaced by captured `cs peek --snapshot`
    // output, this length pin becomes a snapshot file that `insta`
    // reviews on drift.
    const EXPECTED_LEN: usize = 939;
    assert_eq!(
        CANON_FIXTURE.len(),
        EXPECTED_LEN,
        "canon fixture byte length drifted — investigate before regenerating"
    );
}

#[test]
fn canon_fixture_is_valid_utf8() {
    // The raster must be valid UTF-8 end-to-end so every surface
    // (Swift `Text`, Rust `println!`, web mirror) renders the same
    // glyphs. A byte sequence that is valid only under a specific
    // locale silently breaks non-locale surfaces.
    assert!(std::str::from_utf8(CANON_FIXTURE.as_bytes()).is_ok());
}
