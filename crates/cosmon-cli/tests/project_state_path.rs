// SPDX-License-Identifier: AGPL-3.0-only

//! `cs project` must never rewrite a galaxy's tracked surfaces from a state
//! path that is not that galaxy's state directory (issue #171, unit U3).
//!
//! `--config` is the state-directory slot. Passing it a configuration *file*
//! used to load an empty fleet, overwrite the tracked surfaces from it, and
//! only then fail on the frontier and snapshot writes. These tests drive the
//! real `cs` binary and assert the property the operator relies on: a refused
//! run exits non-zero and leaves every tracked file byte-identical.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TRACKED_STATUS: &str = "tracked content, never regenerated\n";

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_CONFIG")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// A galaxy with one `STATUS.md` surface holding hand-written tracked content.
fn galaxy(tmp: &Path) -> PathBuf {
    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"galaxy-ab12\"\n",
    )
    .unwrap();
    fs::write(
        cosmon.join("surfaces.toml"),
        "[[surface]]\nreferent = \"project.status\"\nkind = \"markdown\"\npath = \"STATUS.md\"\n",
    )
    .unwrap();
    fs::write(
        cosmon.join("state/fleet.json"),
        "{\"workers\":{},\"repos\":{}}\n",
    )
    .unwrap();
    fs::write(tmp.join("STATUS.md"), TRACKED_STATUS).unwrap();
    tmp.to_path_buf()
}

fn status_md(root: &Path) -> String {
    fs::read_to_string(root.join("STATUS.md")).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn project_with_a_config_file_is_refused_and_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    let config_file = root.join(".cosmon/config.toml");
    let config_before = fs::read(&config_file).unwrap();

    let out = cs()
        .current_dir(&root)
        .args(["project", "--config"])
        .arg(&config_file)
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "must exit non-zero: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("--config") && stderr(&out).contains("not a directory"),
        "the error must name the flag and the problem: {}",
        stderr(&out)
    );
    assert_eq!(
        status_md(&root),
        TRACKED_STATUS,
        "tracked surface rewritten"
    );
    assert_eq!(fs::read(&config_file).unwrap(), config_before);
    assert!(!root.join(".cosmon/state/frontier.json").exists());
}

#[test]
fn a_state_dir_env_pointing_at_a_file_is_refused_and_names_the_variable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());

    let out = cs()
        .current_dir(&root)
        .env("COSMON_STATE_DIR", root.join(".cosmon/config.toml"))
        .arg("project")
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("COSMON_STATE_DIR"),
        "{}",
        stderr(&out)
    );
    assert_eq!(status_md(&root), TRACKED_STATUS);
}

#[test]
fn a_failing_snapshot_precondition_leaves_the_surfaces_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(tmp.path());
    // The snapshot cannot be written: its path is a directory.
    fs::create_dir(root.join(".cosmon/state/surfaces.snapshot.json")).unwrap();

    let out = cs().current_dir(&root).arg("project").output().unwrap();

    assert!(!out.status.success(), "{}", stderr(&out));
    assert_eq!(
        status_md(&root),
        TRACKED_STATUS,
        "the surface was written before the snapshot precondition was checked"
    );
}

#[test]
fn an_override_state_dir_projects_into_the_walk_up_galaxy_not_its_grandparent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = galaxy(&tmp.path().join("galaxy"));
    // State lives out of tree: its `../..` is `elsewhere/`, which is no galaxy.
    let state = tmp.path().join("elsewhere/a/b");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("fleet.json"), "{\"workers\":{},\"repos\":{}}\n").unwrap();

    let out = cs()
        .current_dir(&root)
        .env("COSMON_STATE_DIR", &state)
        .arg("project")
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", stderr(&out));
    assert_ne!(
        status_md(&root),
        TRACKED_STATUS,
        "galaxy surface not projected"
    );
    assert!(
        !tmp.path().join("elsewhere/a/STATUS.md").exists()
            && !tmp.path().join("elsewhere/a/.cosmon").exists(),
        "projection leaked into the state dir's grandparent"
    );
}

#[test]
fn project_outside_any_galaxy_with_an_unrelated_state_dir_is_a_named_error() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("x/y/state");
    fs::create_dir_all(&state).unwrap();

    let out = cs()
        .current_dir(tmp.path())
        .args(["project", "--config"])
        .arg(&state)
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(stderr(&out).contains("no galaxy found"), "{}", stderr(&out));
    assert!(!tmp.path().join("x/y/.cosmon").exists());
}
