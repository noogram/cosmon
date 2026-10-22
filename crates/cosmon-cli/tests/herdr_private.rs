// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit host qualification; ordinary gates need neither Herdr nor a model.
#![cfg(unix)]

#[test]
#[ignore = "requires Herdr v0.9.5 protocol 22, tmux and configured git identity"]
fn private_herdr_same_name_restart_and_config_switch() -> Result<(), Box<dyn std::error::Error>> {
    let output = tempfile::Builder::new().prefix("hc-").tempdir_in("/tmp")?;
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/herdr-conformance.py");
    let status = std::process::Command::new("python3")
        .arg(script)
        .arg(output.path())
        .arg(env!("CARGO_BIN_EXE_cs"))
        .status()?;
    assert!(
        status.success(),
        "private conformance failed; artifacts: {}",
        output.path().display()
    );
    Ok(())
}
