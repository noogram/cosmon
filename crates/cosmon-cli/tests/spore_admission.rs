// SPDX-License-Identifier: AGPL-3.0-only

//! A feature request cannot allocate the legacy defect spore's DAG.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn feature_request_is_refused_before_any_molecule_exists() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let spore = root.join("spores/cosmon-dev");
    let store = tempfile::tempdir().expect("isolated state store");
    let admission = store.path().join("feature.toml");
    std::fs::write(
        &admission,
        "version = 1\nwork_type = 'feature'\nbaseline = 'HEAD'\ntarget_base = 'HEAD'\n",
    )
    .expect("write admission request");
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(&root)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_STATE_DIR")
        .args(["spore", "run"])
        .arg(spore)
        .args([
            "--var",
            "issue=#143 Add a supported host and service units",
            "--var",
            "affected_ref=HEAD",
            "--var",
            "upstream_version=0.1.0",
            "--allow-unchecked-seal",
        ])
        .arg("--admission")
        .arg(&admission)
        .arg("--store-dir")
        .arg(store.path())
        .output()
        .expect("run spore");
    assert!(!out.status.success(), "a feature must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("plan followed by scoped task-work"),
        "{stderr}"
    );
    let molecules = store.path().join("fleets/default/molecules");
    assert!(
        !molecules.exists()
            || std::fs::read_dir(molecules)
                .expect("read state")
                .next()
                .is_none(),
        "refused admission must create zero molecules"
    );
}

#[test]
fn defect_preflight_emits_pinned_admission_before_dry_run_calls() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let spore = root.join("spores/cosmon-dev");
    let dir = tempfile::tempdir().expect("admission input home");
    let admission = dir.path().join("defect.toml");
    std::fs::write(&admission, concat!(
        "version = 1\nwork_type = 'defect'\nbaseline = 'HEAD'\ntarget_base = 'HEAD'\n",
        "reporter_symptom = 'observed failure'\n",
        "reporter_environment = 'released version and host'\n",
        "reporter_transcript = 'reproduction transcript'\n",
        "paths = ['crates/cosmon-cli/src/cmd/spore.rs']\n",
        "reviewer_capabilities = ['independent refuter', 'distinct provider family']\n",
        "execution_substrates = ['clean-room replay', 'packaged rehearsal', 'published install route']\n",
    )).expect("write admission input");
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(&root)
        .args(["--json", "spore", "validate"])
        .arg(spore)
        .args([
            "--var",
            "issue=observed failure",
            "--var",
            "affected_ref=HEAD",
            "--var",
            "upstream_version=0.1.0",
        ])
        .arg("--admission")
        .arg(admission)
        .output()
        .expect("validate spore");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .expect("admission record")
        .to_owned();
    let json: serde_json::Value = serde_json::from_str(&first).expect("JSON record");
    assert_eq!(json["admission"]["work_type"], "defect");
    assert_eq!(json["admission"]["risk"], "normal");
    assert_eq!(json["admission"]["expected_molecules"], 14);
    assert_eq!(
        json["admission"]["baseline"].as_str().map(str::len),
        Some(40)
    );
}
