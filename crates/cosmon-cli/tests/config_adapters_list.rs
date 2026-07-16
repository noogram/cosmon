// SPDX-License-Identifier: AGPL-3.0-only

//! `cs config adapters [--json]` — discoverability projection of the
//! adapter dispatch registry.
//!
//! Sibling of `config_adapter_provider_override.rs`: the latter pins
//! the *resolution* path (`cs config show adapters` reports the
//! effective `api_key_env` / `base_url` / `default_model` per
//! Direct-API adapter); this file pins the *enumeration* path
//! (`cs config adapters` lists every name the validator will accept).
//! Both surfaces are projections of the same union — built-in adapter
//! names ∪ `[adapters.<name>]` rows from `.cosmon/config.toml` — but
//! they answer different operator questions and therefore exist as
//! distinct verbs.
//!
//! What this test locks in:
//!
//! 1. The wire envelope carries the stable `cs.adapters.list/v1`
//!    schema slug per tolnay's API-minimalism rule on versioned JSON
//!    surfaces.
//! 2. A built-in adapter (`claude`) is reported as `built_in: true,
//!    toml: false` when no TOML row shadows it.
//! 3. A built-in adapter with a TOML override (`openai`) is reported
//!    as `built_in: true, toml: true` — *not* two separate rows, *not*
//!    a silent drop of the built-in provenance.
//! 4. A purely TOML-declared adapter (`internal-llm`) surfaces with
//!    `built_in: false, toml: true` so the operator has authoritative
//!    proof the row was loaded.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    cmd
}

/// Build a project whose `.cosmon/config.toml` carries one built-in
/// override (`openai` → xAI) and one TOML-only adapter
/// (`internal-llm`). Exercises every projection cell in a single
/// fixture so the test reads as a single matrix rather than three
/// near-duplicates.
fn setup_project_with_mixed_adapters() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        r#"
[project]
project_id = "config-adapters-list-test"

[adapters.openai]
api_key_env = "XAI_API_KEY"
base_url = "https://api.x.ai"
default_model = "grok-3"

[adapters.internal-llm]
"#,
    )
    .unwrap();
    tmp
}

/// End-to-end: `cs --json config adapters` carries the versioned
/// envelope and the correct provenance flags for every projection
/// cell (built-in alone, built-in + TOML override, TOML-only).
#[test]
fn config_adapters_json_envelope_carries_union_with_provenance() {
    let tmp = setup_project_with_mixed_adapters();
    let output = cosmon_bin_in(tmp.path())
        .args(["--json", "config", "adapters"])
        .output()
        .expect("cs config adapters failed to spawn");
    assert!(
        output.status.success(),
        "cs config adapters failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let raw = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(raw.trim()).expect("--json output must parse");

    // (1) Stable wire slug — pin the literal so a rename breaks here.
    assert_eq!(json["schema"], "cs.adapters.list/v1");

    // (2) config_path is always present — even when it does not exist
    //     (this fixture wrote it explicitly, so it does exist).
    let cfg_path = json["config_path"].as_str().expect("config_path is string");
    assert!(cfg_path.ends_with("config.toml"), "got: {cfg_path}");

    // Lookup helper — pull a row out of the adapters array by name.
    let adapters = json["adapters"].as_array().expect("adapters is JSON array");
    let row_for = |name: &str| -> serde_json::Value {
        adapters
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no row for {name} in {raw}"))
            .clone()
    };

    // (3) Pure built-in — claude shipped in-tree, no TOML row.
    let claude = row_for("claude");
    assert_eq!(claude["built_in"], true);
    assert_eq!(claude["toml"], false);

    // (4) Built-in *with* TOML override — openai shows up exactly once
    //     with both flags true. Single row, not duplicated.
    let openai_rows: Vec<&serde_json::Value> =
        adapters.iter().filter(|r| r["name"] == "openai").collect();
    assert_eq!(openai_rows.len(), 1, "openai must not be duplicated");
    let openai = openai_rows[0];
    assert_eq!(openai["built_in"], true);
    assert_eq!(openai["toml"], true);

    // (5) Purely TOML-declared — internal-llm not shipped in-tree, only
    //     in config.toml. Operator proof the row was loaded.
    let internal = row_for("internal-llm");
    assert_eq!(internal["built_in"], false);
    assert_eq!(internal["toml"], true);
}

/// Sanity: the human-readable surface (no `--json`) carries the same
/// rows and the table header. We don't snapshot the column widths
/// (those are presentation concerns covered by the unit tests) — only
/// that every projection cell appears in the rendered table.
#[test]
fn config_adapters_human_table_lists_every_name() {
    let tmp = setup_project_with_mixed_adapters();
    let output = cosmon_bin_in(tmp.path())
        .args(["config", "adapters"])
        .output()
        .expect("cs config adapters failed to spawn");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ADAPTER"), "header missing:\n{stdout}");
    assert!(stdout.contains("BUILT_IN"), "header missing:\n{stdout}");
    assert!(stdout.contains("TOML"), "header missing:\n{stdout}");
    // One row per projection cell — anchor on the name column so a
    // header collision is impossible.
    for name in ["claude", "openai", "internal-llm"] {
        assert!(
            stdout.lines().any(|l| l.starts_with(name)),
            "row for {name} missing:\n{stdout}"
        );
    }
}
