// SPDX-License-Identifier: AGPL-3.0-only

//! Property-based invariants for the registry backends.

use std::fmt::Write;

use cosmon_registry::{GalaxyIndex, TomlGalaxyIndex};
use proptest::prelude::*;

fn galaxy_name_strategy() -> impl Strategy<Value = String> {
    // Conservative alphabet: lowercase ASCII + dash. Matches the
    // canonical galaxy naming convention and keeps the TOML
    // serializer from having to handle quoting edge cases.
    "[a-z][a-z0-9-]{0,15}"
}

proptest! {
    /// Every galaxy returned by `list()` must round-trip through
    /// `resolve(name)` and come back bitwise-equal.
    #[test]
    fn resolve_matches_list(
        names in proptest::collection::vec(galaxy_name_strategy(), 0..8)
    ) {
        // Dedupe to avoid duplicate PK in the TOML.
        let mut unique: Vec<String> = names;
        unique.sort();
        unique.dedup();

        let mut body = String::new();
        for (i, n) in unique.iter().enumerate() {
            let _ = write!(body, "[[galaxy]]\nname = \"{n}\"\npath = \"/p/{i}\"\n\n");
        }

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("galaxies.toml");
        std::fs::write(&p, &body).unwrap();

        let idx = TomlGalaxyIndex::load_from(&p).unwrap();
        let listed = idx.list();
        prop_assert_eq!(listed.len(), unique.len());
        for g in listed {
            let resolved = idx.resolve(&g.name);
            prop_assert_eq!(resolved, Some(g));
        }
    }
}
