// SPDX-License-Identifier: AGPL-3.0-only
//! `docs/adr/INDEX.md` says of itself *"auto-generated from docs/adr/ — edit
//! the source, not this file"*. Until 2026-08-07 nothing regenerated it: the
//! renderer existed in this crate, but `cs reconcile` dropped the
//! `project.decisions` surface out of its classification loop before it ever
//! reached `project_surfaces`. The index drifted seven ADRs behind and every
//! one of its 130 links pointed at `docs/adr/docs/adr/…`.
//!
//! A regenerating command is not enough on its own — nothing makes anyone run
//! it. This test is the gate: it re-renders the index from the ADR directory
//! and compares bytes with what is committed. Adding an ADR without running
//! `cs project` (`cs reconcile`) now fails a gate in the same change, which is
//! the only moment the omission is cheap to fix.
//!
//! Decidable from a bare clone: it reads `docs/adr/` and needs no fleet
//! state, no network and no `cs` binary.

use std::path::{Path, PathBuf};

/// The repository root, reached from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(Path::parent) // repo root
        .expect("crate manifest dir has two ancestors")
        .to_path_buf()
}

#[test]
fn committed_adr_index_matches_a_fresh_render() {
    let root = repo_root();
    let index = root.join("docs/adr/INDEX.md");
    let committed = std::fs::read_to_string(&index)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", index.display()));

    // `HostNative` is the branding `.cosmon/surfaces.toml` projects this
    // repository's own surfaces with — the banner in the committed file.
    let fresh = cosmon_surface::render_adr_index_content(
        &root,
        "docs/adr/",
        cosmon_surface::Branding::HostNative,
    );

    assert_eq!(
        committed,
        fresh,
        "docs/adr/INDEX.md is stale — regenerate it with `cs project` \
         (from the repository root) and commit the result. It is a generated \
         file: editing it by hand is undone by the next projection."
    );
}
