// SPDX-License-Identifier: AGPL-3.0-only

//! The `cosmon` alias.
//!
//! The installer poses `cosmon` as a symlink to `cosmon-remote`; the
//! binary renders help and usage under the name it was invoked as.
//! Two pins: the alias face shows `cosmon`, and the canonical name
//! keeps producing the EXACT golden bytes (the alias is additive,
//! never a rename).

#![cfg(unix)]

use std::process::Command;

fn help_via_name(link_name: &str) -> Vec<u8> {
    let tmp = tempfile::tempdir().unwrap();
    let alias = tmp.path().join(link_name);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_cosmon-remote"), &alias).unwrap();
    let out = Command::new(&alias)
        .arg("--help")
        .output()
        .expect("spawn alias");
    assert!(out.status.success());
    out.stdout
}

#[test]
fn alias_renders_help_under_the_invoked_name() {
    let help = String::from_utf8(help_via_name("cosmon")).unwrap();
    assert!(
        help.contains("Usage: cosmon [OPTIONS] <COMMAND>"),
        "alias usage line must show the invoked name, got:\n{help}"
    );
    assert!(
        !help.contains("Usage: cosmon-remote"),
        "alias face must not leak the long name in usage"
    );
}

#[test]
fn canonical_name_is_byte_identical_to_the_golden() {
    let golden = std::fs::read(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/root.help.txt"),
    )
    .unwrap();
    assert_eq!(
        help_via_name("cosmon-remote"),
        golden,
        "invoking through the canonical name must reproduce the golden bytes"
    );
}
