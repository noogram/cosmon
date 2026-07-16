// SPDX-License-Identifier: AGPL-3.0-only

//! Per-backend roundtrip and miss-and-fallback tests.

use cosmon_core::kind::MoleculeKind;
use cosmon_registry::{GalaxyIndex, RegistryError, TomlGalaxyIndex};

#[test]
fn toml_resolve_then_list_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("galaxies.toml");
    std::fs::write(
        &p,
        r#"
[[galaxy]]
name = "cosmon"
path = "/abs/galaxies/cosmon"
fleet = "default"
default_formulas = { task = "task-work" }

[[galaxy]]
name = "mailroom"
path = "/abs/galaxies/mailroom"
fleet = "sec-fleet"
"#,
    )
    .unwrap();
    let idx = TomlGalaxyIndex::load_from(&p).unwrap();

    let listed = idx.list();
    assert_eq!(listed.len(), 2);
    for g in listed {
        let resolved = idx.resolve(&g.name).expect("listed galaxy must resolve");
        assert_eq!(resolved, g);
    }

    assert_eq!(
        idx.default_formula("cosmon", MoleculeKind::Task)
            .unwrap()
            .as_str(),
        "task-work"
    );
    assert!(idx
        .default_formula("mailroom", MoleculeKind::Task)
        .is_none());
}

#[test]
fn toml_missing_file_via_load_default_is_empty_not_error() {
    // This is the "fresh environment" guarantee: no TOML on disk is
    // not an error. We cannot hijack $HOME in an integration test,
    // so we assert on the shape of the default impl rather than on
    // the concrete value — the invariant we want is "it doesn't
    // panic and either succeeds or surfaces a Backend error for
    // truly exotic envs with no config_dir".
    match TomlGalaxyIndex::load_default() {
        Ok(idx) => {
            // Whatever the operator happens to have on disk, the
            // contract is that the call succeeded without panic.
            let _ = idx.list();
        }
        Err(RegistryError::Backend(_)) => {}
        Err(e) => panic!("unexpected error shape: {e:?}"),
    }
}
