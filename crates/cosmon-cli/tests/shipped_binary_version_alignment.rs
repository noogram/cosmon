// SPDX-License-Identifier: AGPL-3.0-only

//! Shipped-binary version alignment gate.
//!
//! **The defect this exists to prevent.** A fresh public install of `v0.2.1`
//! placed two binaries on the user's PATH. `cs --version` said `0.2.1`.
//! `cosmon-remote --version` said `0.3.0` — from an asset literally named
//! `cosmon-remote-0.2.1-*`. A user who downloads `0.2.1` and is told by the
//! installer's own "Next steps" to run `cosmon-remote --version` reads a
//! different number and reasonably concludes the install is broken.
//!
//! **Why it happened, and why prose could not stop it.** The
//! `[workspace.package]` doctrine in the root `Cargo.toml` is sound: the
//! workspace version is the version a user sees, and library crates keep
//! independent semver (ADR forgemaster B6 — do *not* force-synchronize the
//! workspace). But that rule's *premise* was that `cs` is the only binary a
//! user ever sees. task-20260717-c487 (`cosmon-rpp-adapter`, `cs-oidc-mock`)
//! and task-20260718-ff92 (`cosmon-remote`) invalidated the premise without
//! touching the comment, so the comment still read "today: `cosmon-cli`" and
//! still called those crates internal-only. Both statements were false, and
//! nothing noticed — because a comment is not a gate.
//!
//! **What this test enforces**, from the canon at
//! `packaging/shipped-binaries.txt`:
//!
//! 1. Every crate that ships a user-facing binary opts into
//!    `version.workspace = true`, so its `--version` is the release version by
//!    construction rather than by remembering.
//! 2. `release.yml`'s `BINS=` line lists exactly the canon's binaries — no
//!    binary ships unchecked, and no canon row describes a binary that stopped
//!    shipping.
//!
//! It is deliberately a *static* test rather than only a release-time check:
//! the release-time counterpart (`scripts/release-version-conformance.sh`)
//! runs the real binaries, but it runs at tag time, which is far too late to
//! learn that a version is wrong. This one fails in `cargo test --workspace`,
//! on the branch that introduces the drift.
//!
//! Library crates are untouched by all of this and keep their own versions;
//! that is B6 preserved, not reversed.

use std::fs;
use std::path::{Path, PathBuf};

/// Root of the git workspace, derived from this crate's manifest dir.
fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/cosmon-cli/. Walk up two.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cosmon-cli has two ancestors")
        .to_path_buf()
}

/// One row of `packaging/shipped-binaries.txt`.
struct ShippedBinary {
    /// Binary name as it lands on the user's PATH (e.g. `cosmon-remote`).
    binary: String,
    /// Crate that declares the `[[bin]]` (e.g. `cosmon-remote`).
    crate_name: String,
}

/// Parse the canon, skipping comments and blank lines.
///
/// Refuses an ill-formed row loudly: a silently-dropped row is a binary that
/// ships unchecked, which is the exact failure mode this canon exists to close.
fn read_canon() -> Vec<ShippedBinary> {
    let path = workspace_root().join("packaging/shipped-binaries.txt");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

    let mut rows = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(binary), Some(crate_name)) = (fields.next(), fields.next()) else {
            panic!(
                "{}:{}: malformed row {line:?} — expected `<binary> <crate> <tarball>`",
                path.display(),
                idx + 1
            );
        };
        rows.push(ShippedBinary {
            binary: binary.to_string(),
            crate_name: crate_name.to_string(),
        });
    }

    assert!(
        !rows.is_empty(),
        "{} listed no binaries — a canon that gates nothing is worse than none",
        path.display()
    );
    rows
}

/// Every crate shipping a user-facing binary must inherit the workspace
/// (= release) version.
#[test]
fn shipped_binary_crates_inherit_the_release_version() {
    let root = workspace_root();

    for row in read_canon() {
        let manifest_path = root.join("crates").join(&row.crate_name).join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path).unwrap_or_else(|err| {
            panic!(
                "{} ships `{}` but its manifest is unreadable: {err}",
                row.crate_name, row.binary
            )
        });

        // Look only at the `[package]` table: a `version.workspace = true`
        // appearing under `[dependencies]` would otherwise satisfy the check
        // while the package version stayed pinned.
        let package_table = manifest
            .split("\n[")
            .next()
            .expect("split always yields a first element");

        let inherits = package_table.lines().map(str::trim).any(|line| {
            line.starts_with("version.workspace") || line == "version = { workspace = true }"
        });

        assert!(
            inherits,
            "crate `{}` ships the user-facing binary `{}` but pins its own version.\n\
             A user who downloads release X and runs `{} --version` must read X.\n\
             Fix: replace the `version = \"...\"` line in {} with `version.workspace = true`.\n\
             (If `{}` is NOT actually shipped to users, remove its row from \
             packaging/shipped-binaries.txt instead.)",
            row.crate_name,
            row.binary,
            row.binary,
            manifest_path.display(),
            row.binary,
        );
    }
}

/// The release workflow must build exactly the canon's binaries.
///
/// Guards the other direction: aligning the versions is useless if a *new*
/// binary starts shipping via `release.yml` without a canon row, because then
/// neither this test nor the release-time conformance script ever looks at it.
#[test]
fn release_workflow_ships_exactly_the_canon_binaries() {
    let root = workspace_root();
    let workflow_path = root.join(".github/workflows/release.yml");
    let workflow = fs::read_to_string(&workflow_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", workflow_path.display()));

    // The single `BINS="--bin a --bin b ..."` assignment in the build step.
    let bins_line = workflow
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("BINS="))
        .unwrap_or_else(|| {
            panic!(
                "{} has no `BINS=` line — the build step was restructured; \
                 update this test to read the new shape rather than deleting it",
                workflow_path.display()
            )
        });

    let mut shipped: Vec<String> = bins_line
        .trim_start_matches("BINS=")
        .trim_matches('"')
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|pair| pair[0] == "--bin")
        .map(|pair| pair[1].to_string())
        .collect();
    shipped.sort();

    let mut canon: Vec<String> = read_canon().into_iter().map(|row| row.binary).collect();
    canon.sort();

    assert_eq!(
        canon, shipped,
        "packaging/shipped-binaries.txt and release.yml's `BINS=` disagree.\n\
         canon:      {canon:?}\n\
         release.yml:{shipped:?}\n\
         A binary in release.yml but not the canon ships with its version \
         unchecked — that is how `cosmon-remote 0.3.0` reached a v0.2.1 install."
    );
}
