// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sessions seats --all-galaxies --json`, through the real binary
//! (issue #201).
//!
//! Two galaxies sit under one cluster root; only one has a pilot lease. The
//! document must name both, with their roots, and name the holder of the one
//! that has a lease. The authority is the existing seats derivation: nothing
//! here is decided by the all-galaxies path itself.

use std::fs;
use std::path::Path;
use std::process::Command;

use cosmon_minisign_testkit::Operator;

const MISSION: &str = "task-20261008-seat";
const HOLDER: &str = "claude-holder-201";

fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "COSMON_TAKEOVER_PUBKEY",
        "CB_SESSION_ROLE",
        "CB_DEPTH",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn operator() -> Operator {
    Operator::from_seed(11)
}

fn ok(out: &std::process::Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A galaxy under `cluster`, with the operator's trust root pinned.
fn galaxy(cluster: &Path, name: &str) -> std::path::PathBuf {
    let root = cluster.join(name);
    fs::create_dir_all(&root).unwrap();
    ok(&cs(&root).arg("init").output().unwrap(), "init");
    fs::write(
        root.join(".cosmon").join("takeover.pub"),
        operator().public_key_file(),
    )
    .unwrap();
    root
}

/// The operator grants `MISSION` to `HOLDER` and the holder publishes its claim.
fn seat_holder(root: &Path) {
    let challenge = cs(root)
        .args([
            "sessions",
            "takeover",
            "challenge",
            "--mission",
            MISSION,
            "--to",
            HOLDER,
            "--by",
            "operator",
        ])
        .output()
        .unwrap();
    ok(&challenge, "challenge");
    let sig = root.join("grant.minisig");
    fs::write(&sig, operator().sign(&challenge.stdout)).unwrap();
    let granted = cs(root)
        .args([
            "sessions",
            "takeover",
            "grant",
            "--mission",
            MISSION,
            "--to",
            HOLDER,
            "--by",
            "operator",
            "--attestation",
        ])
        .arg(&sig)
        .output()
        .unwrap();
    ok(&granted, "grant");
    let ping = cs(root)
        .args([
            "presence",
            "ping",
            "--session",
            HOLDER,
            "--role",
            "primary",
            "--mission",
            MISSION,
            "--epoch",
            "1",
        ])
        .output()
        .unwrap();
    ok(&ping, "primary ping");
}

#[test]
fn all_galaxies_document_names_both_galaxies_and_the_holder() {
    let tmp = tempfile::tempdir().unwrap();
    let cluster = tmp.path();
    let leased = galaxy(cluster, "leased");
    let bare = galaxy(cluster, "bare");
    seat_holder(&leased);

    let out = cs(&bare)
        .env("COSMON_CLUSTER_ROOT", cluster)
        .args(["--json", "sessions", "seats", "--all-galaxies"])
        .output()
        .unwrap();
    ok(&out, "seats --all-galaxies");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let galaxies = doc["galaxies"].as_array().unwrap();
    assert_eq!(galaxies.len(), 2, "{doc}");
    let named = |name: &str| galaxies.iter().find(|g| g["galaxy"] == name).unwrap();

    let bare_entry = named("bare");
    assert_eq!(bare_entry["root"], bare.to_str().unwrap());
    assert_eq!(bare_entry["state"], "no mission");

    let leased_entry = named("leased");
    assert_eq!(leased_entry["root"], leased.to_str().unwrap());
    let mission = &leased_entry["missions"][0];
    assert_eq!(mission["mission"], MISSION);
    assert_eq!(mission["verdict"], "one");
    assert_eq!(mission["seats"][0]["session"], HOLDER);
    assert_eq!(mission["seats"][0]["standing"], "holder");
    assert_eq!(mission["seats"][0]["epoch"], 1);
}
