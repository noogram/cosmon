// SPDX-License-Identifier: AGPL-3.0-only

//! `cs reconcile` repairs a `state.json` that holds no content and refuses one
//! that does (issue #171, unit U1b). A refusal never stops the repair of the
//! other molecules, and the command exits non-zero naming the refused path.

use std::fs;
use std::path::Path;
use std::process::Command;

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_state::event_log::EventLogWriter;

const EMPTY_ID: &str = "task-20261006-aaa1";
const LEGACY_ID: &str = "task-20261006-bbb2";
const LEGACY_BYTES: &[u8] = br#"{"id":"task-20261006-bbb2"}"#;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_CONFIG")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn molecule_dir(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".cosmon/state/fleets/default/molecules").join(id)
}

/// A galaxy whose log nucleates both molecules; one `state.json` is empty and
/// the other is a legacy object missing required fields.
fn galaxy(root: &Path) {
    let cosmon = root.join(".cosmon");
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

    let mut log = EventLogWriter::open(&cosmon.join("state/events.jsonl")).unwrap();
    for id in [EMPTY_ID, LEGACY_ID] {
        log.emit(
            EventV2::MoleculeNucleated {
                molecule_id: MoleculeId::new(id).unwrap(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            },
            None,
        )
        .unwrap();
    }
    log.sync().unwrap();

    let empty = molecule_dir(root, EMPTY_ID);
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("state.json"), b"").unwrap();
    let legacy = molecule_dir(root, LEGACY_ID);
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("state.json"), LEGACY_BYTES).unwrap();
}

#[test]
fn reconcile_rebuilds_the_empty_file_and_refuses_the_legacy_one() {
    let tmp = tempfile::tempdir().unwrap();
    galaxy(tmp.path());

    let out = cs()
        .current_dir(tmp.path())
        .arg("reconcile")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "must exit non-zero: {stderr}");

    let legacy_path = molecule_dir(tmp.path(), LEGACY_ID).join("state.json");
    assert!(
        stderr.contains(&legacy_path.display().to_string()),
        "stderr must name the refused path {}: {stderr}",
        legacy_path.display()
    );
    assert_eq!(fs::read(&legacy_path).unwrap(), LEGACY_BYTES);
    assert!(
        !molecule_dir(tmp.path(), LEGACY_ID)
            .join("state.json.broken.1")
            .exists(),
        "a refusal must not archive"
    );

    let empty_dir = molecule_dir(tmp.path(), EMPTY_ID);
    let rebuilt: serde_json::Value =
        serde_json::from_slice(&fs::read(empty_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(rebuilt["id"], EMPTY_ID);
    assert_eq!(
        fs::read(empty_dir.join("state.json.broken.1")).unwrap(),
        b""
    );
}
