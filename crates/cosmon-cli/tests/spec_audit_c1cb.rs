// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test — `cs spec-audit` surfaces the c1cb Gödel-class bug.
//!
//! Scenario: a molecule was nucleated
//! and its feature branch landed on `main` without a sanctioned
//! `cs done` sequence — the morning-merge witness. The ledger alone
//! cannot detect this (no `Done` event was written), so the audit
//! consults a git-topology probe and flags the drift.
//!
//! This test locks in recall against the class. If `cs spec-audit` ever
//! stops reporting `bypass_merge` in this exact scenario, the whole
//! Chantier 3 deliverable is regressed.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use cosmon_core::event_v2::{Envelope, EventV2, Seq};
use cosmon_core::id::MoleculeId;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// Build a tiny git repository whose `main` branch contains the target
/// `feat/<mol>` branch — i.e. the feature branch has already been merged.
///
/// The audit's git probe runs `git merge-base --is-ancestor feat/<mol>
/// main`; returning exit 0 is the c1cb witness. We avoid any remote
/// configuration and pass `--target-ref main` so no network work is
/// needed.
fn seed_merged_repo(root: &Path, mol: &MoleculeId) {
    // Minimal init: repo, one commit on main, a feat branch that
    // fast-forward merges, and we end back on main.
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "audit-test")
            .env("GIT_AUTHOR_EMAIL", "audit@example.invalid")
            .env("GIT_COMMITTER_NAME", "audit-test")
            .env("GIT_COMMITTER_EMAIL", "audit@example.invalid")
            .status()
            .expect("git invocation failed");
        assert!(status.success(), "git {args:?} failed");
    };

    git(&["init", "-q", "--initial-branch=main"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(root.join("seed.txt"), "seed").unwrap();
    git(&["add", "seed.txt"]);
    git(&["commit", "-q", "-m", "seed"]);

    let feat = format!("feat/{}", mol.as_str());
    git(&["checkout", "-q", "-b", &feat]);
    fs::write(root.join("work.txt"), "done").unwrap();
    git(&["add", "work.txt"]);
    git(&["commit", "-q", "-m", "work"]);
    git(&["checkout", "-q", "main"]);
    git(&["merge", "-q", "--no-edit", &feat]);
}

fn write_events(path: &Path, envelopes: &[Envelope]) {
    let mut f = fs::File::create(path).unwrap();
    for env in envelopes {
        writeln!(f, "{}", serde_json::to_string(env).unwrap()).unwrap();
    }
}

/// The core c1cb reproducer: only a Nucleated event is in the ledger,
/// but git topology says the feature branch is merged. The audit must
/// flag `bypass_merge`.
#[test]
fn spec_audit_flags_bypass_merge_for_c1cb_witness() {
    let tmp = tempfile::tempdir().unwrap();
    let mol = MoleculeId::new("cs-20260419-c1cb").unwrap();

    seed_merged_repo(tmp.path(), &mol);

    let events = tmp.path().join("events.jsonl");
    write_events(
        &events,
        &[Envelope::new(
            Seq(0),
            None,
            EventV2::MoleculeNucleated {
                molecule_id: mol.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: Vec::new(),
            },
        )],
    );

    let output = cosmon_bin()
        .arg("--json")
        .arg("spec-audit")
        .arg("--events")
        .arg(&events)
        .arg("--repo")
        .arg(tmp.path())
        .arg("--target-ref")
        .arg("main")
        .output()
        .expect("failed to invoke cs spec-audit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Non-zero exit because the audit found a drift.
    assert!(
        !output.status.success(),
        "expected non-zero exit for drift; stdout={stdout} stderr={stderr}"
    );

    // Machine-readable report carries a bypass_merge drift for the mol.
    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is NDJSON report");
    let drifts = report["drifts"].as_array().unwrap();
    let bypass = drifts
        .iter()
        .find(|d| d["kind"] == "bypass_merge")
        .expect("bypass_merge drift missing from report");
    assert_eq!(
        bypass["molecule_id"],
        serde_json::Value::String(mol.as_str().to_owned())
    );
}

/// Clean scenario: the molecule went through the full lifecycle and the
/// merge landed via a sanctioned `Done` action. No drift expected.
#[test]
fn spec_audit_is_clean_on_sanctioned_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let mol = MoleculeId::new("cs-20260419-c1ea").unwrap();

    seed_merged_repo(tmp.path(), &mol);

    let events = tmp.path().join("events.jsonl");
    let nuc = Envelope::new(
        Seq(0),
        None,
        EventV2::MoleculeNucleated {
            molecule_id: mol.clone(),
            formula_id: "task-work".into(),
            parent_id: None,
            blocks: Vec::new(),
        },
    );
    let tackled = Envelope::new(
        Seq(1),
        None,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol.clone(),
            from: "pending".into(),
            to: "running".into(),
        },
    );
    let evolved = Envelope::new(
        Seq(2),
        None,
        EventV2::MoleculeStepCompleted {
            molecule_id: mol.clone(),
            step: 0,
            total: 1,
            duration_ms: Some(10),
            step_hash: None,
        },
    );
    let completed = Envelope::new(
        Seq(3),
        None,
        EventV2::MoleculeCompleted {
            molecule_id: mol.clone(),
            duration_ms: Some(20),
            reason: "ok".into(),
        },
    );
    let merged = Envelope::new(
        Seq(4),
        None,
        EventV2::MergeCompleted {
            molecule: mol.clone(),
            branch: format!("feat/{}", mol.as_str()),
            result: cosmon_core::event_v2::MergeResult::Ok,
            federation_provenance: None,
        },
    );

    write_events(&events, &[nuc, tackled, evolved, completed, merged]);

    let output = cosmon_bin()
        .arg("--json")
        .arg("spec-audit")
        .arg("--events")
        .arg(&events)
        .arg("--repo")
        .arg(tmp.path())
        .arg("--target-ref")
        .arg("main")
        .output()
        .expect("failed to invoke cs spec-audit");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected clean exit; stdout={stdout} stderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["drifts"].as_array().unwrap().len(), 0);
}
