// SPDX-License-Identifier: AGPL-3.0-only

//! Integration coverage for the `exec_command` egress jail (autonomy guard).
//!
//! The unit-level command construction is covered in `cosmon-core::egress`;
//! this file pins the *wiring*: the tool reads `COSMON_EGRESS_POLICY` from the
//! environment (now **fail-closed** — security-review 5008), and a strict
//! policy does not break ordinary local commands on a host that can only
//! enforce advisorily (the macOS dev host, or a hardened Linux kernel that
//! degrades under C1-F3). On a netns-capable kernel the same call path spawns
//! the shell inside an egress-denied namespace; that branch is asserted at the
//! construction layer in the core crate's `autonomy_attacks` corpus and the
//! `exec_command_netns_e2e` container harness rather than by reaching the
//! network from CI.
//!
//! The env-mutating spawn phases live in **one** `#[test]` so the
//! `COSMON_EGRESS_POLICY` mutation cannot race a sibling test thread (cargo
//! runs a file's tests concurrently) — the same discipline as
//! `exec_command_netns_e2e.rs`.

use std::path::Path;

use cosmon_agent_harness::tool::Tool;
use cosmon_agent_harness::{ExecCommand, ExecResult};
use cosmon_core::egress::EgressPolicy;
use tempfile::tempdir;

fn exec(tool: &ExecCommand, work_dir: &Path, cmd: &str) -> ExecResult {
    let args = serde_json::json!({ "command": cmd }).to_string();
    let raw = tool
        .execute(&args, work_dir)
        .expect("exec must return an envelope");
    serde_json::from_str(&raw).expect("result is valid JSON")
}

/// Fail-closed default (security-review 5008): an **unset** policy resolves to
/// `DenyExternal`, not `AllowAll`. A dropped env must never silently open
/// egress — only an explicit `allow-all` does. Asserted at the policy layer so
/// it holds on every host regardless of enforcement mode.
#[test]
fn unset_policy_fail_closes_to_deny_external() {
    assert_eq!(
        EgressPolicy::from_env_value(None),
        EgressPolicy::DenyExternal,
        "unset COSMON_EGRESS_POLICY must fail closed to deny-external"
    );
    assert_eq!(
        EgressPolicy::from_env_value(Some("garbage")),
        EgressPolicy::DenyExternal,
        "a corrupt token must fail closed to deny-external"
    );
}

/// The env-mutating spawn phases, kept sequential in one test.
#[test]
fn egress_wiring_spawn_phases() {
    let dir = tempdir().unwrap();

    // ---- phase 1: explicit allow-all is the only unconfined shell ----------
    // A plain local command runs byte-identically to the pre-guard
    // `/bin/bash --noprofile --norc` shape.
    std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::AllowAll.token());
    let r = exec(&ExecCommand::new(), dir.path(), "echo guarded-ok");
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.output.trim(), "guarded-ok");
    std::env::remove_var(EgressPolicy::ENV_VAR);

    // ---- phase 2: strict deny-external still runs local commands -----------
    // On macOS the enforcement mode is Advisory (no kernel netns), so the shell
    // runs unwrapped; the point is that the env plumbing engages without
    // breaking the local path. On Linux the same path would spawn under
    // `unshare --net`, which a sandboxed CI may refuse — hence the macOS gate
    // keeps the assertion deterministic.
    #[cfg(target_os = "macos")]
    {
        std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::DenyExternal.token());
        let r = exec(&ExecCommand::new(), dir.path(), "echo still-local");
        std::env::remove_var(EgressPolicy::ENV_VAR);
        assert_eq!(r.exit_code, 0, "local echo must still run: {r:?}");
        assert!(r.output.contains("still-local"));
    }
}

/// The policy token the tool reads is the same token the spawner writes —
/// the contract between `cs tackle` and `exec_command`.
#[test]
fn env_var_name_and_tokens_are_the_shared_contract() {
    assert_eq!(EgressPolicy::ENV_VAR, "COSMON_EGRESS_POLICY");
    assert_eq!(EgressPolicy::DenyExternal.token(), "deny-external");
    assert_eq!(
        EgressPolicy::from_env_value(Some("deny-external")),
        EgressPolicy::DenyExternal
    );
}
