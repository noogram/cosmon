// SPDX-License-Identifier: AGPL-3.0-only

//! The in-process `exec_command` shell must not inherit its parent's
//! environment (issue #151, unit W3).
//!
//! The property: a credential variable, a custom-named provider credential
//! variable and a shell-startup variable seeded in the parent process are
//! **absent** from the tool shell, on the first spawn and again after the
//! shell died and was respawned. Presence is probed by name (`${VAR+set}`),
//! so no value is ever printed and no real secret is involved.
//!
//! Also pinned: an allowlisted variable and a persistent in-shell export still
//! work, the lifecycle context a worker's `cs` needs still arrives, and an
//! unset egress policy reaches the shell as an explicit `deny-external`.
//!
//! One `#[test]` mutates the process environment, so the phases are
//! sequential and cannot race a sibling test thread.

use std::path::Path;

use cosmon_agent_harness::tool::Tool;
use cosmon_agent_harness::{ExecCommand, ExecResult};
use tempfile::tempdir;

const DEFAULT_CREDENTIAL: &str = "OPENAI_API_KEY";
const CUSTOM_CREDENTIAL: &str = "ACME_PROVIDER_SYNTHETIC_TOKEN";
const STARTUP_VAR: &str = "BASH_ENV";
const SENTINEL: &str = "synthetic-sentinel-value-0xC0FFEE";

fn exec(tool: &ExecCommand, work_dir: &Path, cmd: &str) -> ExecResult {
    let args = serde_json::json!({ "command": cmd }).to_string();
    let raw = tool
        .execute(&args, work_dir)
        .expect("exec must return an envelope");
    serde_json::from_str(&raw).expect("result is valid JSON")
}

/// Names (never values) among `names` that the tool shell can see.
fn visible(tool: &ExecCommand, work_dir: &Path, names: &[&str]) -> Vec<String> {
    let probe: String = names
        .iter()
        .map(|n| format!("[ -n \"${{{n}+set}}\" ] && echo {n}; true\n"))
        .collect();
    let out = exec(tool, work_dir, &probe);
    assert!(
        !out.output.contains(SENTINEL),
        "a sentinel value reached the tool output: {}",
        out.output
    );
    out.output
        .lines()
        .filter(|l| !l.starts_with("[shell restarted"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn tool_shell_does_not_inherit_parent_environment() {
    let dir = tempdir().unwrap();
    let wd = dir.path();

    // SAFETY-by-discipline: single sequential test in this binary.
    std::env::set_var(cosmon_core::egress::EgressPolicy::ENV_VAR, "allow-all");
    std::env::set_var(DEFAULT_CREDENTIAL, SENTINEL);
    std::env::set_var(CUSTOM_CREDENTIAL, SENTINEL);
    std::env::set_var(STARTUP_VAR, "/nonexistent/synthetic-startup-file");
    std::env::set_var("COSMON_MOL_DIR", "/synthetic/mol/dir");
    std::env::set_var("LANG", "C");

    let secret_names = [DEFAULT_CREDENTIAL, CUSTOM_CREDENTIAL, STARTUP_VAR];
    let tool = ExecCommand::new();

    // Phase 1: first spawn.
    assert_eq!(
        visible(&tool, wd, &secret_names),
        Vec::<String>::new(),
        "credential / startup variables leaked into the first shell"
    );

    // Allowlisted context still arrives; a persistent export still works.
    let ctx = visible(
        &tool,
        wd,
        &["PATH", "HOME", "COSMON_MOL_DIR", "COSMON_EGRESS_POLICY"],
    );
    assert_eq!(
        ctx,
        ["PATH", "HOME", "COSMON_MOL_DIR", "COSMON_EGRESS_POLICY"],
        "required runtime context missing from the shell"
    );
    let _ = exec(&tool, wd, "export SHELL_LOCAL_EXPORT=kept");
    assert_eq!(
        exec(&tool, wd, "echo $SHELL_LOCAL_EXPORT").output.trim(),
        "kept"
    );

    // Phase 2: respawn after the shell dies.
    let _ = exec(&tool, wd, "exit 3");
    assert_eq!(
        visible(&tool, wd, &secret_names),
        Vec::<String>::new(),
        "credential / startup variables leaked into the respawned shell"
    );
    // The in-shell export belonged to the dead shell; the parent env is not
    // re-imported to restore it.
    assert!(exec(&tool, wd, "echo \"[$SHELL_LOCAL_EXPORT]\"")
        .output
        .trim_end()
        .ends_with("[]"));

    // Phase 3: an unset egress policy reaches the shell as explicit deny.
    std::env::remove_var(cosmon_core::egress::EgressPolicy::ENV_VAR);
    std::env::set_var(cosmon_core::egress::REQUIRE_NETNS_ENV, "0");
    let tool = ExecCommand::new();
    let out = exec(&tool, wd, "echo \"$COSMON_EGRESS_POLICY\"");
    if out.exit_code == 0 {
        assert_eq!(out.output.trim(), "deny-external");
    }
}
