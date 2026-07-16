// SPDX-License-Identifier: AGPL-3.0-only

//! `[adapters.openai].api_key_env` shuts the silent-leak trap closed.
//!
//! Regression test for GAP #4 from the academy smoke chronicle
//! `2026-05-18-grok-direct-api-smoke-result.md`. Before this fix, an
//! operator running the `openai` adapter against xAI had to manually
//! `env -u OPENAI_API_KEY XAI_API_KEY=… cs tackle` because the
//! historical scan (first non-empty of `OPENAI_API_KEY`,
//! `XAI_API_KEY`, `MOONSHOT_API_KEY`) silently picked the wrong
//! credential when both were set in the parent shell. A
//! request meant for `api.x.ai` could then route to
//! `api.openai.com` with a Grok model identifier — a 404 from the
//! wrong vendor, not data exfiltration, but the silent-failure
//! class is the problem.
//!
//! The fix: `[adapters.openai].api_key_env = "XAI_API_KEY"` makes
//! the binding authoritative — the multi-vendor scan is skipped, the
//! declared env var is the *only* source. This test exercises the
//! end-to-end resolution by running `cs config show adapters --json`
//! against a project whose `.cosmon/config.toml` carries the xAI
//! binding, with `OPENAI_API_KEY` *and* `XAI_API_KEY` both set in
//! the test process environment.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cosmon_bin_in(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    cmd
}

/// Build a project tempdir whose `.cosmon/config.toml` declares the
/// `openai` adapter routed to xAI via the new schema knobs.
fn setup_project_with_xai_openai() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        r#"
[project]
project_id = "config-adapter-provider-override-test"

[adapters.openai]
api_key_env = "XAI_API_KEY"
base_url = "https://api.x.ai"
default_model = "grok-3"
"#,
    )
    .unwrap();
    tmp
}

/// With `[adapters.openai].api_key_env = "XAI_API_KEY"` declared,
/// `cs config show adapters --json` must report `XAI_API_KEY` as the
/// resolved key env (provenance: `config`) even when `OPENAI_API_KEY`
/// is also set in the parent shell. The silent-leak trap is closed
/// by the config tier shadowing the env-tier scan.
#[test]
fn config_show_adapters_reports_xai_when_config_says_xai() {
    let tmp = setup_project_with_xai_openai();
    let output = cosmon_bin_in(tmp.path())
        .env("OPENAI_API_KEY", "sk-openai-decoy")
        .env("XAI_API_KEY", "xai-real-key")
        .args(["--json", "config", "show", "adapters"])
        .output()
        .expect("cs config show adapters failed to spawn");
    assert!(
        output.status.success(),
        "cs config show adapters failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
            .expect("--json output must parse");
    let rows = json.as_array().expect("output is a JSON array");
    let openai = rows
        .iter()
        .find(|r| r["adapter"] == "openai")
        .expect("openai row present");
    assert_eq!(
        openai["api_key_env"], "XAI_API_KEY",
        "config tier must override env scan: {openai}"
    );
    assert_eq!(
        openai["api_key_source"], "config",
        "provenance must be 'config' when [adapters.openai].api_key_env is set"
    );
    assert_eq!(openai["api_key_present"], true);
    assert_eq!(openai["base_url"], "https://api.x.ai");
    assert_eq!(openai["base_url_source"], "config");
    assert_eq!(openai["default_model"], "grok-3");
    assert_eq!(openai["default_model_source"], "config");
}

/// Symmetric anthropic case: no config row at all, `ANTHROPIC_API_KEY`
/// unset → fall through to the compile-time default with `api_key_present
/// = false`. This is the trap diagnostic the operator looks for before
/// kicking off a `cs tackle --adapter anthropic`.
#[test]
fn config_show_adapters_anthropic_defaults_when_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let cosmon_dir = tmp.path().join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"config-adapter-anthropic-defaults\"\n",
    )
    .unwrap();

    let output = cosmon_bin_in(tmp.path())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_MODEL")
        .args(["--json", "config", "show", "adapters"])
        .output()
        .expect("cs config show adapters failed to spawn");
    assert!(output.status.success());
    let json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let anthropic = json
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["adapter"] == "anthropic")
        .expect("anthropic row present");
    assert_eq!(anthropic["api_key_env"], "ANTHROPIC_API_KEY");
    assert_eq!(anthropic["api_key_source"], "default");
    assert_eq!(anthropic["api_key_present"], false);
    assert_eq!(anthropic["base_url"], "https://api.anthropic.com");
    assert_eq!(anthropic["base_url_source"], "default");
    assert_eq!(anthropic["default_model"], "claude-opus-4-7");
    assert_eq!(anthropic["default_model_source"], "default");
}
