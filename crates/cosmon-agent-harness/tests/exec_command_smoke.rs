// SPDX-License-Identifier: AGPL-3.0-only

//! Smoke test: drive `cargo --version` through the persistent
//! `exec_command` shell and assert the version line lands in
//! `output`. This is the end-to-end witness the briefing's
//! "Definition of done" asks for — proof that the spawned shell can
//! reach real tools on PATH and the marker protocol survives a
//! several-line response.

use std::path::Path;

use cosmon_agent_harness::{ExecCommand, ExecResult, Tool};
use cosmon_core::egress::EgressPolicy;
use tempfile::tempdir;

/// Pin the egress policy to `allow-all` for this binary, once. These smoke
/// tests exercise the persistent shell reaching real tools on PATH, not the
/// egress jail. Since the security-review 5008 fix an unset policy fails closed
/// to `deny-external`, which on a netns-capable Linux host would wrap the shell
/// in `unshare --net` (unavailable in sandboxed CI). `Once` makes it a barrier
/// that never races.
fn allow_local_shell() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::AllowAll.token());
    });
}

fn exec(tool: &ExecCommand, work_dir: &Path, cmd: &str) -> ExecResult {
    allow_local_shell();
    let args = serde_json::json!({"command": cmd}).to_string();
    let raw = tool.execute(&args, work_dir).expect("exec must succeed");
    serde_json::from_str(&raw).expect("result is valid JSON")
}

#[test]
fn cargo_version_runs_through_persistent_shell() {
    let dir = tempdir().expect("tempdir");
    let tool = ExecCommand::new();
    let result = exec(&tool, dir.path(), "cargo --version");
    assert_eq!(
        result.exit_code, 0,
        "cargo --version must return 0; got {result:?}"
    );
    assert!(
        result.output.contains("cargo"),
        "output should mention cargo; got {:?}",
        result.output
    );
    assert!(!result.timed_out, "must not time out: {result:?}");
}

#[test]
fn harness_can_check_then_write_then_check_again() {
    // The briefing's headline acceptance criterion: "harness can run
    // cargo check, edit a file, run cargo check again, all within one
    // tackle." We exercise the persistence side here — same tool
    // instance threads three commands through the same shell — using
    // shell builtins for portability (no cargo project needed in the
    // tempdir).
    let dir = tempdir().expect("tempdir");
    let tool = ExecCommand::new();

    let r1 = exec(&tool, dir.path(), "echo first && pwd");
    assert_eq!(r1.exit_code, 0, "first call must succeed: {r1:?}");

    let r2 = exec(
        &tool,
        dir.path(),
        "printf 'hello\\n' > note.txt && cat note.txt",
    );
    assert_eq!(r2.exit_code, 0, "write+read must succeed: {r2:?}");
    assert_eq!(r2.output.trim(), "hello");

    let r3 = exec(&tool, dir.path(), "ls -1 note.txt && echo third");
    assert_eq!(r3.exit_code, 0, "third call must succeed: {r3:?}");
    assert!(r3.output.contains("note.txt"));
    assert!(r3.output.contains("third"));
}
