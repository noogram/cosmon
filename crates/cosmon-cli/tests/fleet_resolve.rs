// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `cs fleet resolve` — load-time fleet composition.
//!
//! Covers the ADR-038/ADR-039 v0 acceptance test ("scenario 1") and a handful
//! of negative paths (duplicate agent, missing child file, bad URI scheme).

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// Scenario 1 (wiki + dev + blob) — the acceptance test for the
/// "1-minute success criterion".
///
/// Three child fleet files, three `[[fleet.include]]` entries, four agents
/// total. `cs fleet resolve --json | jq '.fleet.agents | length'` must emit `4`.
#[test]
fn scenario_1_wiki_dev_blob_resolves_to_four_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();

    fs::write(
        root.join("fleets/wiki.toml"),
        r#"[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();

    fs::write(
        root.join("fleets/dev.toml"),
        r#"[fleet]
schema_version = 1
id = "dev"

[[agents]]
name = "coder"
role = "implementation"
clearance = "write"

[[agents]]
name = "reviewer"
role = "advisory"
clearance = "read"
"#,
    )
    .unwrap();

    fs::write(
        root.join("fleets/blob.toml"),
        r#"[fleet]
schema_version = 1
id = "blob"

[[agents]]
name = "observer"
role = "advisory"
clearance = "read"
"#,
    )
    .unwrap();

    fs::write(
        root.join("fleet.toml"),
        r#"[fleet]
schema_version = 1
id = "test"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[fleet.include]]
source = "file:./fleets/dev.toml"

[[fleet.include]]
source = "file:./fleets/blob.toml"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("--json")
        .arg("fleet")
        .arg("resolve")
        .arg(root.join("fleet.toml"))
        .output()
        .expect("cs fleet resolve failed");

    assert!(
        out.status.success(),
        "cs fleet resolve failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON parse");
    let agents = value
        .get("fleet")
        .and_then(|f| f.get("agents"))
        .and_then(|a| a.as_array())
        .expect("fleet.agents array");
    assert_eq!(agents.len(), 4, "expected 4 agents, got {}", agents.len());

    // Provenance preserved — every agent has origin_fleet_id.
    let names: Vec<&str> = agents.iter().filter_map(|a| a["name"].as_str()).collect();
    assert!(names.contains(&"editor"));
    assert!(names.contains(&"coder"));
    assert!(names.contains(&"reviewer"));
    assert!(names.contains(&"observer"));
    for a in agents {
        let origin = a["origin_fleet_id"].as_str().unwrap_or("");
        assert!(
            ["wiki", "dev", "blob", "test"].contains(&origin),
            "agent {:?} has unexpected origin_fleet_id {:?}",
            a["name"],
            origin
        );
    }

    // fleet.id preserved on the composite.
    assert_eq!(value["fleet"]["id"].as_str(), Some("test"));
    assert_eq!(value["fleet"]["schema_version"].as_u64(), Some(1));
}

/// Backward compatibility — a pre-ADR-038 monolithic fleet.toml (no
/// `[fleet]` block, no `[[fleet.include]]`) must still resolve unchanged.
#[test]
fn monolithic_fleet_still_resolves_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(
        &path,
        r#"fleet = "legacy"
version = 1

[[agents]]
name = "alpha"
role = "implementation"
clearance = "write"

[[agents]]
name = "beta"
role = "advisory"
clearance = "read"

[[channels]]
from = "alpha"
to = "beta"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("--json")
        .arg("fleet")
        .arg("resolve")
        .arg(&path)
        .output()
        .expect("cs fleet resolve failed");

    assert!(
        out.status.success(),
        "cs fleet resolve failed on monolithic file: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON parse");
    assert_eq!(value["fleet"]["id"].as_str(), Some("legacy"));
    let agents = value["fleet"]["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
}

/// Duplicate agent id across master + child must hard-fail with an error
/// message naming both fleets (the error string is a user-facing deliverable,
/// per feynman). Both source paths must appear in stderr.
#[test]
fn duplicate_agent_across_fleets_hard_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();
    fs::write(
        root.join("fleets/wiki.toml"),
        r#"[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();
    fs::write(
        root.join("fleet.toml"),
        r#"[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"

[[agents]]
name = "editor"
role = "advisory"
clearance = "read"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(root.join("fleet.toml"))
        .output()
        .expect("run");
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("duplicate agent"), "stderr={stderr}");
    assert!(stderr.contains("editor"), "stderr={stderr}");
    assert!(
        stderr.contains("wiki") && stderr.contains("master"),
        "stderr must name both fleets: {stderr}"
    );
    assert!(
        stderr.contains("as = "),
        "stderr should suggest --as rename: {stderr}"
    );
}

/// `as =` prefix rescues a collision and namespaces the child's agent.
#[test]
fn as_prefix_resolves_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();
    fs::write(
        root.join("fleets/wiki.toml"),
        r#"[fleet]
schema_version = 1
id = "wiki"

[[agents]]
name = "editor"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();
    fs::write(
        root.join("fleet.toml"),
        r#"[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/wiki.toml"
as = "wiki"

[[agents]]
name = "editor"
role = "advisory"
clearance = "read"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("--json")
        .arg("fleet")
        .arg("resolve")
        .arg(root.join("fleet.toml"))
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON parse");
    let names: Vec<&str> = value["fleet"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(names.contains(&"wiki:editor"));
    assert!(names.contains(&"editor"));
}

/// v0 resolver accepts only `file:` scheme. Other schemes must parse-but-error.
#[test]
fn non_file_scheme_errors_with_not_implemented_in_v0() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(
        &path,
        r#"[fleet]
schema_version = 1
id = "m"

[[fleet.include]]
source = "git+https://example.com/wiki.toml"

[[agents]]
name = "x"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(&path)
        .output()
        .expect("run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not implemented in v0"), "stderr={stderr}");
}

/// Transitive includes are rejected in v0 (godel — undecidable by reduction).
#[test]
fn transitive_include_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();
    fs::write(
        root.join("fleets/leaf.toml"),
        r#"[fleet]
schema_version = 1
id = "leaf"

[[agents]]
name = "l"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();
    fs::write(
        root.join("fleets/mid.toml"),
        r#"[fleet]
schema_version = 1
id = "mid"

[[fleet.include]]
source = "file:./leaf.toml"

[[agents]]
name = "m"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();
    fs::write(
        root.join("fleet.toml"),
        r#"[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/mid.toml"

[[agents]]
name = "root"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(root.join("fleet.toml"))
        .output()
        .expect("run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("transitive includes"), "stderr={stderr}");
}

/// Missing child file produces a readable error naming the include and
/// the master file.
#[test]
fn missing_include_file_reports_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(
        &path,
        r#"[fleet]
schema_version = 1
id = "m"

[[fleet.include]]
source = "file:./does-not-exist.toml"

[[agents]]
name = "x"
role = "implementation"
clearance = "write"
"#,
    )
    .unwrap();
    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(&path)
        .output()
        .expect("run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to read included fleet"),
        "stderr={stderr}"
    );
}

/// `cs fleet resolve --help` exists and documents the command.
#[test]
fn fleet_resolve_appears_in_help() {
    let out = cosmon_bin()
        .arg("fleet")
        .arg("--help")
        .output()
        .expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("resolve"),
        "fleet subcommand list should contain `resolve`: {stdout}"
    );
}
