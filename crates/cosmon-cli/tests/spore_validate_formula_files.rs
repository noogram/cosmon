// SPDX-License-Identifier: AGPL-3.0-only

//! `cs spore validate` must resolve every formula file a spore node
//! references, and fail naming the node and the missing file when one is
//! absent (issue #173). Before this check the manifest validated and the
//! error only surfaced at dispatch, when `cs spore run` tried to load it.

use std::process::{Command, Output};

use tempfile::TempDir;

const MANIFEST: &str = r#"
[spore]
name = "fixture"
version = 1

[spore.formulas.work]
path = "work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
"#;

const FORMULA: &str = r#"
formula = "work"
version = 1
id_prefix = "task"

[[steps]]
id = "do"
title = "Do the work"
description = "the body"
acceptance = "any evidence"
"#;

fn validate(dir: &TempDir) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_FORMULAS_DIR")
        .args(["spore", "validate"])
        .arg(dir.path().join("spore.toml"))
        .output()
        .expect("run cs spore validate")
}

fn fixture(with_formula: bool) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("spore.toml"), MANIFEST).unwrap();
    if with_formula {
        std::fs::write(dir.path().join("work.formula.toml"), FORMULA).unwrap();
    }
    dir
}

#[test]
fn a_missing_formula_file_fails_validation_naming_node_and_file() {
    let dir = fixture(false);
    let out = validate(&dir);
    assert!(
        !out.status.success(),
        "validate must exit non-zero when a referenced formula file is absent"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("frame"), "names the node: {stderr}");
    assert!(
        stderr.contains("work.formula.toml"),
        "names the missing file: {stderr}"
    );
}

#[test]
fn the_same_spore_with_the_formula_present_validates() {
    let dir = fixture(true);
    let out = validate(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
