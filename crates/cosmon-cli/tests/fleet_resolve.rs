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

// ---------------------------------------------------------------------------
// Keys the resolver does not carry are never lost in silence
// ---------------------------------------------------------------------------

/// The minimised reproduction of the pilot-lab report: an agent that
/// declares `adapter` next to `model`.
const ADAPTER_FIXTURE: &str = include_str!("fixtures/fleet_adapter_agent.toml");

fn resolve_json(path: &std::path::Path) -> serde_json::Value {
    let out = cosmon_bin()
        .arg("--json")
        .arg("fleet")
        .arg("resolve")
        .arg(path)
        .output()
        .expect("run cs fleet resolve");
    assert!(
        out.status.success(),
        "resolve failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("resolve --json emits JSON")
}

/// `adapter` is an agent field: it reaches the flattened output, per agent,
/// next to `model`, and an agent that declares none carries `null`.
#[test]
fn agent_adapter_survives_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(&path, ADAPTER_FIXTURE).unwrap();

    let value = resolve_json(&path);
    let agents = value["fleet"]["agents"].as_array().expect("agents array");
    let by_name = |n: &str| {
        agents
            .iter()
            .find(|a| a["name"] == n)
            .unwrap_or_else(|| panic!("agent {n} missing"))
    };
    assert_eq!(by_name("implementer")["adapter"], "codex");
    assert_eq!(by_name("implementer")["model"], "gpt-6.1-sol");
    assert_eq!(by_name("mission-lead")["adapter"], "claude");
    assert!(by_name("unpinned")["adapter"].is_null());
}

/// The plain-text listing shows the pin too, so a human reading it does not
/// conclude the key was dropped.
#[test]
fn agent_adapter_shown_in_plain_output() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(&path, ADAPTER_FIXTURE).unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(&path)
        .output()
        .expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("implementer"))
        .expect("implementer line");
    assert!(line.contains("adapter=codex"), "line={line}");
}

/// With `--strict`, an agent key outside the schema is refused with the key,
/// the agent and the file named — not dropped.
#[test]
fn unknown_agent_key_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    // `adaptr` is a typo of `adapter`: the exact silent loss the resolver
    // used to hide.
    fs::write(
        &path,
        ADAPTER_FIXTURE.replace("adapter = \"codex\"", "adaptr = \"codex\""),
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg("--strict")
        .arg(&path)
        .output()
        .expect("run");
    assert!(!out.status.success(), "unknown key must fail the resolve");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`adaptr`"), "key not named: {stderr}");
    assert!(
        stderr.contains("agent `implementer`"),
        "agent not named: {stderr}"
    );
    assert!(
        stderr.contains(&path.display().to_string()),
        "file not named: {stderr}"
    );
}

/// Strictness is not limited to agents: with `--strict` any table the schema
/// defines refuses keys it does not carry (here the top level).
#[test]
fn unknown_top_level_key_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(
        &path,
        format!("{ADAPTER_FIXTURE}\n[parameters]\nmission = \"x\"\n"),
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg("--strict")
        .arg(&path)
        .output()
        .expect("run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`parameters`"), "key not named: {stderr}");
    assert!(stderr.contains("top level"), "table not named: {stderr}");
}

/// With `--strict` an included child is held to the same rule, and the error names the
/// child's file rather than the master's.
#[test]
fn unknown_key_in_included_child_names_the_child_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();
    fs::write(
        root.join("fleets/dev.toml"),
        r#"[fleet]
schema_version = 1
id = "dev"

[[agents]]
name = "coder"
role = "implementation"
clearance = "write"
harness = "codex"
"#,
    )
    .unwrap();
    fs::write(
        root.join("fleet.toml"),
        r#"[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/dev.toml"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg("--strict")
        .arg(root.join("fleet.toml"))
        .output()
        .expect("run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("`harness`"), "key not named: {stderr}");
    assert!(
        stderr.contains("agent `coder`"),
        "agent not named: {stderr}"
    );
    assert!(
        stderr.contains("dev.toml"),
        "child file not named: {stderr}"
    );
}

/// By default a key the schema does not carry is a warning, not a refusal:
/// the resolve succeeds, and stderr names each dropped key, its agent or
/// table, and its file — in the master and in an included child alike.
#[test]
fn unknown_keys_warn_by_name_and_exit_zero_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("fleets")).unwrap();
    fs::write(
        root.join("fleets/dev.toml"),
        r#"[fleet]
schema_version = 1
id = "dev"

[[agents]]
name = "coder"
role = "implementation"
clearance = "write"
fan = 3
"#,
    )
    .unwrap();
    let master = root.join("fleet.toml");
    fs::write(
        &master,
        r#"[fleet]
schema_version = 1
id = "master"

[[fleet.include]]
source = "file:./fleets/dev.toml"

[[agents]]
name = "lead"
role = "orchestration"
clearance = "write"
adapter = "codex"
tier = "frontier"

[parameters]
mission = "x"
"#,
    )
    .unwrap();

    let out = cosmon_bin()
        .arg("--json")
        .arg("fleet")
        .arg("resolve")
        .arg(&master)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "default resolve must succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let master_label = master.display().to_string();
    let warns = |key: &str, location: &str, file: &str| {
        stderr.lines().any(|l| {
            l.starts_with("warning:")
                && l.contains(&format!("`{key}`"))
                && l.contains(location)
                && l.contains(file)
        })
    };
    assert!(warns("tier", "agent `lead`", &master_label), "{stderr}");
    assert!(warns("parameters", "top level", &master_label), "{stderr}");
    assert!(warns("fan", "agent `coder`", "dev.toml"), "{stderr}");
    assert_eq!(stderr.matches("warning:").count(), 3, "{stderr}");

    // The carried keys still resolve, and stdout stays pure JSON.
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let agents = value["fleet"]["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|a| a["adapter"] == "codex"));
}

/// A fleet file that stays inside the schema produces no warning at all.
#[test]
fn clean_fleet_resolves_without_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    fs::write(&path, ADAPTER_FIXTURE).unwrap();
    let out = cosmon_bin()
        .arg("fleet")
        .arg("resolve")
        .arg(&path)
        .output()
        .expect("run");
    assert!(out.status.success());
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("warning:"),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Cosmon's own fleet templates and examples resolve (warnings allowed) —
/// the resolver must stay usable on the files the project itself ships.
#[test]
fn cosmon_own_templates_resolve() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for entry in fs::read_dir(repo.join("templates")).expect("templates dir") {
        let dir = entry.unwrap().path();
        let f = dir.join("fleet.toml");
        if f.is_file() {
            files.push(f);
        }
    }
    for entry in fs::read_dir(repo.join("examples")).expect("examples dir") {
        let f = entry.unwrap().path();
        if f.to_string_lossy().ends_with(".fleet.toml") {
            files.push(f);
        }
    }
    assert!(files.len() >= 5, "expected shipped fleets, found {files:?}");
    for f in files {
        if f.ends_with("templates/formal-research/fleet.toml") {
            // Pre-existing and unrelated to unknown keys: an agent declares
            // `role = "observation"`, which is not an `AgentRole`, so the
            // file never parsed. Pinned as failing for that reason, so the
            // day the template is fixed this arm fails and must be removed.
            let out = cosmon_bin()
                .arg("fleet")
                .arg("resolve")
                .arg(&f)
                .output()
                .expect("run");
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("unknown role: observation"),
                "formal-research changed behaviour: drop this exemption: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            continue;
        }
        let out = cosmon_bin()
            .arg("fleet")
            .arg("resolve")
            .arg(&f)
            .output()
            .expect("run");
        assert!(
            out.status.success(),
            "{} failed to resolve: {}",
            f.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
