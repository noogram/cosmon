// SPDX-License-Identifier: AGPL-3.0-only

//! A deployed client identifies the exact source commit on `--version`.

use std::process::Command;

#[test]
fn version_includes_the_source_commit() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cosmon-remote"))
        .arg("--version")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).unwrap();
    assert!(
        version.contains(&format!("cosmon-remote {} (", env!("CARGO_PKG_VERSION"))),
        "{version}"
    );

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..").canonicalize().unwrap();
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(manifest)
        .output()
        .unwrap();
    let owns_source = top.status.success()
        && std::path::Path::new(String::from_utf8(top.stdout).unwrap().trim())
            .canonicalize()
            .is_ok_and(|path| path == root);
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(manifest)
        .output()
        .unwrap();
    if owns_source && commit.status.success() {
        let sha = String::from_utf8(commit.stdout).unwrap();
        assert!(version.contains(sha.trim()), "{version}");
    } else {
        assert!(version.contains("(unknown)"), "{version}");
    }
}
