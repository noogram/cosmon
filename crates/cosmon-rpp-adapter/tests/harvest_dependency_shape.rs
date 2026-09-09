// SPDX-License-Identifier: AGPL-3.0-only

//! The adapter depends on the harvest **transaction**, not on the CLI.
//!
//! The whole point of lifting `cs done` out of `cosmon-cli` was that the RPP
//! route could call it. The cheap way to get there would have been to add
//! `cosmon-cli` to this crate's `[dependencies]` — the transaction was already
//! reachable from its `cosmon_cli` lib target. That would have worked, and it
//! would have made every clap subcommand, every TUI widget, every provider
//! adapter and the `ratatui` stack part of the server's build and attack
//! surface, to reach one function.
//!
//! So the shape is the decision, and this is what pins it: from
//! `cosmon-rpp-adapter`'s manifest, following path dependencies transitively,
//! `cosmon-harvest` is reachable and `cosmon-cli` is not.
//!
//! It reads manifests rather than shelling out to `cargo tree`, for two
//! reasons: the workspace forbids tests that trigger an implicit cargo build
//! (`no_implicit_cargo_build_in_tests`), and a manifest walk names the edge
//! that would have to be added, which is the thing a reviewer needs to see.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// Path-dependency names declared by one manifest, in any of the three
/// dependency tables. Dev-dependencies are included on purpose: a test-only
/// edge to `cosmon-cli` would still drag the CLI into this crate's build
/// graph every time someone runs `cargo test -p cosmon-rpp-adapter`.
fn path_deps(manifest: &Path) -> Vec<String> {
    let body = std::fs::read_to_string(manifest).unwrap_or_default();
    let doc: toml::Value =
        toml::from_str(&body).unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let mut out = Vec::new();
    for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(t) = doc.get(table).and_then(toml::Value::as_table) else {
            continue;
        };
        out.extend(t.keys().cloned());
    }
    out
}

/// Resolve a workspace member's manifest by crate name, or `None` for an
/// external (registry) dependency, which cannot lead back to `cosmon-cli`.
fn member_manifest(crates_dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = crates_dir.join(name).join("Cargo.toml");
    candidate.is_file().then_some(candidate)
}

#[test]
fn the_adapter_reaches_the_harvest_without_reaching_the_cli() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .to_path_buf();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back("cosmon-rpp-adapter".to_owned());

    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(manifest) = member_manifest(&crates_dir, &name) else {
            continue;
        };
        for dep in path_deps(&manifest) {
            queue.push_back(dep);
        }
    }

    assert!(
        seen.contains("cosmon-harvest"),
        "the adapter must reach the harvest transaction as a library; without \
         that edge `POST /v1/molecules/{{id}}/done` is back to spawning `cs` \
         or refusing 501"
    );
    assert!(
        !seen.contains("cosmon-cli"),
        "the adapter must NOT reach `cosmon-cli`: the transaction was lifted \
         into its own crate precisely so that a server does not compile the \
         whole CLI to close a molecule. Reachable set: {seen:?}"
    );
}
