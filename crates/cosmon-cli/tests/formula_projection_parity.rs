// SPDX-License-Identifier: AGPL-3.0-only

//! One canonical source, several projections — enforced for formula files.
//!
//! A formula that lives inside a spore (`spores/<name>/formulas/`) and is ALSO
//! reachable by name from the repo (`.cosmon/formulas/`, what
//! `cs nucleate <name>` resolves) is one rule stored at two paths. Two
//! independently hand-edited copies of one rule are two copies that drift, and
//! the one that drifts is the one somebody reads as the original.
//!
//! This happened. `converge-clean-room` diverged for a month across 355 lines:
//! the retrospective's decision was applied to the spore copy while
//! `cs nucleate converge-clean-room` kept resolving to the repo-level one. The
//! divergence was invisible in the public history because the repo-level copy
//! entered the tree inside the squashed v0.4.0 release projection, so `git log`
//! on that path shows exactly one commit.
//!
//! The design chosen is BYTE-IDENTICAL PROJECTION: the spore copy is canonical,
//! the repo-level copy is its projection, and both carry the marker sentence
//! naming which is which. Byte equality (rather than a symlink) keeps the spore
//! a self-contained shippable unit on every platform, and keeps the projection
//! checkable with nothing but `read_to_string`.
//!
//! Two things are asserted, because either alone leaves a way back in:
//!
//! 1. shared BASENAME — resolution is by basename, so a shared basename is the
//!    shape that actually reaches an operator;
//! 2. shared `formula = "<id>"` — a spore file renamed on disk while keeping its
//!    id would slip past a basename-only check and still be the same rule twice.

use std::path::{Path, PathBuf};

/// Workspace root: `crates/cosmon-cli` -> `../..`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `formula = "<id>"` field of a formula TOML, if it declares one.
fn formula_id(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("formula ")?.trim().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_string())
}

/// Every `spores/*/formulas/*.formula.toml` in the workspace, sorted.
fn spore_formulas(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let spores = root.join("spores");
    let Ok(entries) = std::fs::read_dir(&spores) else {
        return found;
    };
    for spore in entries.flatten() {
        let formulas = spore.path().join("formulas");
        let Ok(files) = std::fs::read_dir(&formulas) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".formula.toml"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A spore formula and its repo-level peer must be the same bytes.
///
/// Failing this test does not mean "run a formatter". It means two files claim
/// to be one rule and disagree, so one of them is silently authoritative for
/// whoever happens to read it — decide the merge per axis, then re-project.
#[test]
fn spore_and_repo_formulas_are_byte_identical() {
    let root = repo_root();
    let repo_formulas = root.join(".cosmon/formulas");

    let mut checked = 0usize;
    for spore_path in spore_formulas(&root) {
        let spore_body = std::fs::read_to_string(&spore_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", spore_path.display()));

        // The two ways one rule ends up at two paths: same basename (what
        // `cs nucleate` resolves) or same declared id under a different name.
        let basename = spore_path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("spore formula basename")
            .to_string();
        let mut peers = vec![repo_formulas.join(&basename)];
        if let Some(id) = formula_id(&spore_body) {
            let by_id = repo_formulas.join(format!("{id}.formula.toml"));
            if by_id != peers[0] {
                peers.push(by_id);
            }
        }

        for peer in peers {
            if !peer.exists() {
                continue;
            }
            let peer_body = std::fs::read_to_string(&peer)
                .unwrap_or_else(|error| panic!("read {}: {error}", peer.display()));
            checked += 1;
            assert_eq!(
                spore_body,
                peer_body,
                "formula stored at two paths has diverged:\n  canonical: {}\n  projection: {}\n\
                 {} lines vs {} lines. `cs nucleate` resolves the projection, so a decision \
                 applied only to the canonical copy never reaches an operator. Reconcile per \
                 axis — do not copy one over the other blind — then re-project.",
                spore_path.display(),
                peer.display(),
                spore_body.lines().count(),
                peer_body.lines().count(),
            );
        }
    }

    assert!(
        checked > 0,
        "no spore formula has a repo-level peer under {} — this test would pass vacuously, \
         which is how a parity gate becomes a blind spot",
        repo_formulas.display()
    );
}

/// Both copies must SAY that one of them is a projection.
///
/// Byte parity stops the drift; it does not tell the next editor which file to
/// edit. Without the marker the honest reading of two identical files is "pick
/// either", and picking either is what restarts this clock.
#[test]
fn projected_formulas_name_their_canonical_source() {
    const MARKER: &str = "CANONICAL SOURCE:";
    let root = repo_root();
    let repo_formulas = root.join(".cosmon/formulas");

    for spore_path in spore_formulas(&root) {
        let basename = spore_path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("spore formula basename");
        let peer = repo_formulas.join(basename);
        if !peer.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&spore_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", spore_path.display()));
        assert!(
            body.contains(MARKER),
            "{} is stored at two paths but declares no `{MARKER}` line, so the next editor \
             cannot tell which copy to edit",
            spore_path.display()
        );
    }
}
