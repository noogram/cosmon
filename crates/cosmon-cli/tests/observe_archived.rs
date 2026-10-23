// SPDX-License-Identifier: AGPL-3.0-only

//! Archived-only observation redirects the reader to the existing archive verb.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs(project: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(project)
        .env("COSMON_CONFIG", project.join(".cosmon/config.toml"))
        .env("COSMON_STATE_DIR", project.join(".cosmon/state"));
    cmd
}

#[test]
fn archived_observe_names_archive_show_and_keeps_failure_status() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let id = "task-20261023-abcd";
    let entry = tmp.path().join(".cosmon/state/archive/2026/10").join(id);
    fs::create_dir_all(&entry)?;
    fs::write(
        tmp.path().join(".cosmon/config.toml"),
        "[project]\nproject_id = \"test-observe-archived\"\n",
    )?;
    fs::write(
        entry.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "1", "formula_pin": "task-work@1",
            "molecule_id": id, "status": "completed"
        }))?,
    )?;
    let show = cs(tmp.path()).args(["archive", "show", id]).output()?;
    assert!(show.status.success(), "archive show reads the same fixture");

    for query in [id, "task-20261023-ab"] {
        let output = cs(tmp.path()).args(["observe", query]).output()?;
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("is archived"), "{error}");
        assert!(error.contains(&format!("cs archive show {id}")), "{error}");
    }
    let missing = cs(tmp.path())
        .args(["observe", "task-20261023-ffff"])
        .output()?;
    assert_eq!(missing.status.code(), Some(1));
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("no molecule matching"), "{error}");
    assert!(!error.contains("cs archive show"), "{error}");
    Ok(())
}
