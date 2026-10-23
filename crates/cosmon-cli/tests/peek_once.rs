// SPDX-License-Identifier: AGPL-3.0-only

//! The documented single-pass flag must work with piped output.

use std::fs;
use std::process::{Command, Stdio};

#[test]
fn once_selects_non_tui_in_a_non_tty() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join(".cosmon/state");
    fs::create_dir_all(&state)?;
    let config = tmp.path().join(".cosmon/config.toml");
    fs::write(&config, "[project]\nproject_id = \"test-peek-once\"\n")?;
    let output = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env("COSMON_CONFIG", &config)
        .env("COSMON_STATE_DIR", &state)
        .args(["peek", "--once", "--no-tmux", "--energy-tick-interval", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    assert!(
        output.status.success(),
        "--once should print once without a TTY: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "single-pass baseline is printed");
    Ok(())
}
