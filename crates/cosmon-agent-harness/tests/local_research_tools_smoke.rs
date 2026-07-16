// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end smoke test for the local-research tool extension.
//!
//! Two surfaces:
//!
//! 1. The seven-tool [`default_registry`] dispatches each tool via its
//!    public name and the wire envelope round-trips through JSON. This
//!    is the "mock spine" the briefing's *Acceptance* line asks for —
//!    we exercise the dispatch path the real spine uses
//!    (`ToolRegistry::execute(&ToolCall { ... }, work_dir)`) and
//!    confirm the contract holds for every tool.
//! 2. A realistic local-research sequence: write a fresh file, find
//!    it by glob, list its parent, grep for a known token, read the
//!    body, then edit it. The tools chain naturally because every
//!    one of them honours the same `work_dir` sandbox.

use std::path::Path;

use cosmon_agent_harness::{default_registry, ToolCall};
use cosmon_core::egress::EgressPolicy;
use serde_json::Value;
use tempfile::tempdir;

/// Pin the egress policy to `allow-all` for this binary, once. The
/// `exec_command` tool exercised here reaches a real shell; since the
/// security-review 5008 fix an unset policy fails closed to `deny-external`,
/// which on a netns-capable Linux host would wrap the shell in `unshare --net`
/// (unavailable in sandboxed CI). `Once` makes it a barrier that never races.
fn allow_local_shell() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var(EgressPolicy::ENV_VAR, EgressPolicy::AllowAll.token());
    });
}

fn dispatch(work_dir: &Path, name: &str, args: &Value) -> String {
    allow_local_shell();
    let registry = default_registry();
    let call = ToolCall::new(format!("call-{name}"), name, args.to_string());
    registry.execute(&call, work_dir).expect("dispatch must ok")
}

#[test]
fn default_registry_advertises_seven_tools() {
    let registry = default_registry();
    let names: Vec<&str> = registry.declarations().iter().map(|d| d.name).collect();
    // BTreeMap-stable ordering — declarations come back in
    // alphabetical key order, which is also what the prompt-cache
    // prefix-stability discipline asks for (S5).
    assert_eq!(
        names,
        vec![
            "edit_file",
            "exec_command",
            "find_file",
            "grep",
            "list_dir",
            "read_file",
            "write_file",
        ]
    );
}

#[test]
fn registry_contract_holds_for_every_tool() {
    // For each registered tool, dispatch a minimal valid call and
    // confirm the JSON envelope round-trips. Failure here means a
    // tool was added without honouring the registry contract.
    let dir = tempdir().expect("tempdir");
    let work = dir.path();

    // Seed a tiny worktree so every tool has something to bite.
    std::fs::write(work.join("hello.txt"), "alpha\nbeta\ngamma\n").expect("seed");
    std::fs::create_dir_all(work.join("src")).expect("mkdir");
    std::fs::write(work.join("src/lib.rs"), "fn main() { println!(\"ok\"); }\n").expect("seed");

    // read_file
    let raw = dispatch(
        work,
        "read_file",
        &serde_json::json!({ "path": "hello.txt" }),
    );
    let v: Value = serde_json::from_str(&raw).expect("read_file json");
    assert!(v["content"].as_str().unwrap().contains("alpha"));

    // list_dir
    let raw = dispatch(work, "list_dir", &serde_json::json!({}));
    let v: Value = serde_json::from_str(&raw).expect("list_dir json");
    let paths: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(paths.contains(&"hello.txt"), "got {paths:?}");
    assert!(paths.contains(&"src"), "got {paths:?}");

    // grep
    let raw = dispatch(
        work,
        "grep",
        &serde_json::json!({ "pattern": "beta", "path": "." }),
    );
    let v: Value = serde_json::from_str(&raw).expect("grep json");
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    assert_eq!(v["matches"][0]["path"], "hello.txt");
    assert_eq!(v["matches"][0]["line_number"], 2);

    // find_file
    let raw = dispatch(work, "find_file", &serde_json::json!({ "pattern": "*.rs" }));
    let v: Value = serde_json::from_str(&raw).expect("find_file json");
    assert!(v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "src/lib.rs"));

    // write_file — create a brand-new file
    let raw = dispatch(
        work,
        "write_file",
        &serde_json::json!({ "path": "fresh.md", "content": "# Hello" }),
    );
    let v: Value = serde_json::from_str(&raw).expect("write_file json");
    assert_eq!(v["path"], "fresh.md");
    assert_eq!(v["bytes_written"], 7);
    assert_eq!(
        std::fs::read_to_string(work.join("fresh.md")).unwrap(),
        "# Hello"
    );

    // edit_file — modify the freshly created file
    let raw = dispatch(
        work,
        "edit_file",
        &serde_json::json!({
            "edits": [{
                "path": "fresh.md",
                "search": "# Hello",
                "replace": "# Hello, world"
            }]
        }),
    );
    let v: Value = serde_json::from_str(&raw).expect("edit_file json");
    assert!(v.is_array());
    assert!(v[0]["Ok"].is_object(), "expected Ok variant: {v}");
    assert_eq!(
        std::fs::read_to_string(work.join("fresh.md")).unwrap(),
        "# Hello, world"
    );

    // exec_command — a portable shell builtin so the smoke test does
    // not depend on the host PATH.
    let raw = dispatch(
        work,
        "exec_command",
        &serde_json::json!({ "command": "echo ok" }),
    );
    let v: Value = serde_json::from_str(&raw).expect("exec_command json");
    assert_eq!(v["exit_code"], 0);
    assert!(v["output"].as_str().unwrap().contains("ok"));
}

#[test]
fn unknown_tool_is_rejected_loudly() {
    let dir = tempdir().expect("tempdir");
    let registry = default_registry();
    let call = ToolCall::new("call-bogus", "not_a_real_tool", "{}");
    let err = registry
        .execute(&call, dir.path())
        .expect_err("must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("not whitelisted"),
        "expected NotWhitelisted, got: {msg}"
    );
}

#[test]
fn local_research_sequence_chains_through_sandbox() {
    // A realistic agent flow: write → find → list → grep → read →
    // edit. Every tool honours the same work_dir sandbox; the
    // sequence cannot escape.
    let dir = tempdir().expect("tempdir");
    let work = dir.path();

    // 1. write_file — create a fresh note.
    dispatch(
        work,
        "write_file",
        &serde_json::json!({
            "path": "notes/finding.md",
            "content": "# Finding\n\nThe answer is 42.\n"
        }),
    );

    // 2. find_file — locate it by glob.
    let raw = dispatch(work, "find_file", &serde_json::json!({ "pattern": "*.md" }));
    let v: Value = serde_json::from_str(&raw).unwrap();
    let found: Vec<&str> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(found, vec!["notes/finding.md"]);

    // 3. list_dir — confirm it landed in the right subtree.
    let raw = dispatch(
        work,
        "list_dir",
        &serde_json::json!({ "path": "notes", "recursive": false }),
    );
    let v: Value = serde_json::from_str(&raw).unwrap();
    let entries: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert!(entries.iter().any(|p| p.ends_with("finding.md")));

    // 4. grep — search for a token.
    let raw = dispatch(work, "grep", &serde_json::json!({ "pattern": "answer is" }));
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    assert_eq!(v["matches"][0]["path"], "notes/finding.md");

    // 5. read_file — pull the body back.
    let raw = dispatch(
        work,
        "read_file",
        &serde_json::json!({ "path": "notes/finding.md" }),
    );
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert!(v["content"].as_str().unwrap().contains("answer is 42"));

    // 6. edit_file — refine the note.
    dispatch(
        work,
        "edit_file",
        &serde_json::json!({
            "edits": [{
                "path": "notes/finding.md",
                "search": "42",
                "replace": "still 42 (but bolder now)"
            }]
        }),
    );

    let final_body = std::fs::read_to_string(work.join("notes/finding.md")).unwrap();
    assert!(final_body.contains("still 42 (but bolder now)"));
}

#[test]
fn sandbox_holds_against_path_escape_across_every_tool() {
    // Defense-in-depth witness: every tool that accepts a path arg
    // must refuse `../escape` and `/etc/passwd`. The registry
    // dispatch path is the model-facing surface, so we drive each
    // one through it.
    let dir = tempdir().expect("tempdir");
    let registry = default_registry();
    let work = dir.path();

    let escape_calls: &[(&str, Value)] = &[
        ("read_file", serde_json::json!({ "path": "../escape.txt" })),
        ("list_dir", serde_json::json!({ "path": "/etc" })),
        (
            "grep",
            serde_json::json!({ "pattern": "x", "path": "../escape" }),
        ),
        (
            "find_file",
            serde_json::json!({ "pattern": "*", "path": "/etc" }),
        ),
        (
            "write_file",
            serde_json::json!({ "path": "/etc/owned", "content": "x" }),
        ),
        (
            "edit_file",
            serde_json::json!({ "edits": [{
                "path": "../escape.txt",
                "search": "",
                "replace": "owned"
            }]}),
        ),
    ];

    for (name, args) in escape_calls {
        let call = ToolCall::new(format!("escape-{name}"), *name, args.to_string());
        let err = registry.execute(&call, work).unwrap_err_or_else_for(name);
        let msg = err.to_string();
        assert!(
            msg.contains("path escapes work_dir")
                || msg.contains("absolute path refused")
                || msg.contains("parent-dir escape"),
            "{name} must surface PathEscape, got: {msg}"
        );
    }
}

/// Tiny ergonomic helper — `Result::expect_err` is fine, but the
/// per-tool name in the panic message makes the failure trivially
/// diagnosable.
trait UnwrapErrOrElseFor<E> {
    fn unwrap_err_or_else_for(self, name: &str) -> E;
}

impl<T, E: std::fmt::Debug> UnwrapErrOrElseFor<E> for Result<T, E> {
    fn unwrap_err_or_else_for(self, name: &str) -> E {
        match self {
            Ok(_) => panic!("{name} must refuse path escape"),
            Err(e) => e,
        }
    }
}
