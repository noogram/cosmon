// SPDX-License-Identifier: AGPL-3.0-only

//! File-tool path authority, exercised through the real local registry.
//!
//! Every tool that opens file content or reports directory entries must
//! decide path authority the same way: canonicalise, then require the
//! result to stay inside the canonical work root, failing closed when the
//! canonical form cannot be computed. Each test plants a symlink that
//! points at a synthetic file outside the work root and asserts that the
//! tool neither returns the outside content nor modifies the outside file.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::Path;

use cosmon_agent_harness::{default_registry, ToolCall};
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};

const SECRET: &str = "OUTSIDE-SENTINEL-4f1c";

struct Fixture {
    work: TempDir,
    outside: TempDir,
}

impl Fixture {
    /// A work root holding `inside.txt`, plus an unrelated directory
    /// holding `secret.txt`, linked from the work root in several ways.
    fn new() -> Self {
        let work = tempdir().expect("work");
        let outside = tempdir().expect("outside");
        std::fs::write(work.path().join("inside.txt"), "inside body\n").expect("seed");
        std::fs::write(outside.path().join("secret.txt"), SECRET).expect("secret");
        symlink(
            outside.path().join("secret.txt"),
            work.path().join("final_link"),
        )
        .expect("final link");
        symlink(outside.path(), work.path().join("ancestor_link")).expect("ancestor link");
        symlink(
            outside.path().join("missing.txt"),
            work.path().join("dangling_link"),
        )
        .expect("dangling link");
        Self { work, outside }
    }

    fn call(&self, tool: &str, args: &Value) -> Result<String, String> {
        let registry = default_registry();
        let call = ToolCall::new(format!("call-{tool}"), tool, args.to_string());
        registry
            .execute(&call, self.work.path())
            .map_err(|e| e.to_string())
    }

    fn secret_untouched(&self) -> bool {
        std::fs::read_to_string(self.outside.path().join("secret.txt")).is_ok_and(|b| b == SECRET)
    }
}

fn leaks(result: &Result<String, String>) -> bool {
    matches!(result, Ok(body) if body.contains(SECRET))
}

#[test]
fn read_file_refuses_final_and_ancestor_symlinks_to_outside() {
    let fx = Fixture::new();
    for path in ["final_link", "ancestor_link/secret.txt"] {
        let r = fx.call("read_file", &json!({ "path": path }));
        assert!(r.is_err(), "read_file {path} must be refused, got {r:?}");
    }
}

#[test]
fn read_file_refuses_dangling_link_and_missing_file() {
    let fx = Fixture::new();
    for path in ["dangling_link", "ancestor_link/missing.txt", "nope.txt"] {
        assert!(fx.call("read_file", &json!({ "path": path })).is_err());
    }
}

#[test]
fn read_file_still_reads_inside_the_root() {
    let fx = Fixture::new();
    std::fs::create_dir(fx.work.path().join("sub")).expect("sub");
    std::fs::write(fx.work.path().join("sub/a.txt"), "nested\n").expect("seed");
    symlink("inside.txt", fx.work.path().join("inner_link")).expect("inner link");
    for (path, needle) in [
        ("inside.txt", "inside body"),
        ("sub/a.txt", "nested"),
        ("inner_link", "inside body"),
    ] {
        let body = fx
            .call("read_file", &json!({ "path": path }))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(body.contains(needle), "{path}: {body}");
    }
}

#[test]
fn read_file_refuses_absolute_and_parent_paths() {
    let fx = Fixture::new();
    let abs = fx.outside.path().join("secret.txt");
    for path in [abs.to_string_lossy().into_owned(), "../x".to_owned()] {
        assert!(fx.call("read_file", &json!({ "path": path })).is_err());
    }
}

#[test]
fn grep_find_list_do_not_surface_outside_content_or_entries() {
    let fx = Fixture::new();
    let grep = fx.call(
        "grep",
        &json!({ "pattern": "OUTSIDE-SENTINEL", "path": "." }),
    );
    assert!(!leaks(&grep), "grep leaked: {grep:?}");
    let grep_link = fx.call(
        "grep",
        &json!({ "pattern": "OUTSIDE-SENTINEL", "path": "ancestor_link" }),
    );
    assert!(
        grep_link.is_err(),
        "grep through ancestor link: {grep_link:?}"
    );

    let find = fx
        .call("find_file", &json!({ "pattern": "*.txt", "path": "." }))
        .expect("find");
    assert!(!find.contains("secret.txt"), "find surfaced: {find}");
    assert!(fx
        .call(
            "find_file",
            &json!({ "pattern": "*", "path": "ancestor_link" })
        )
        .is_err());

    assert!(fx
        .call("list_dir", &json!({ "path": "ancestor_link" }))
        .is_err());
    let listing = fx.call("list_dir", &json!({ "path": "." })).expect("list");
    assert!(listing.contains("inside.txt"));
    assert!(!listing.contains("secret.txt"), "listing: {listing}");
}

#[test]
fn list_dir_reports_links_as_metadata_without_traversal() {
    let fx = Fixture::new();
    let listing = fx
        .call("list_dir", &json!({ "path": ".", "recursive": true }))
        .expect("list");
    let v: Value = serde_json::from_str(&listing).expect("json");
    let kind_of = |name: &str| {
        v["entries"]
            .as_array()
            .and_then(|es| es.iter().find(|e| e["path"] == name))
            .map(|e| e["kind"].as_str().unwrap_or("").to_owned())
    };
    assert_eq!(kind_of("ancestor_link").as_deref(), Some("symlink"));
    assert_eq!(kind_of("final_link").as_deref(), Some("symlink"));
    assert!(kind_of("ancestor_link/secret.txt").is_none());
}

#[test]
fn edit_and_write_refuse_links_and_leave_outside_file_intact() {
    let fx = Fixture::new();
    let edit = |path: &str| {
        fx.call(
            "edit_file",
            &json!({ "edits": [{ "path": path, "search": SECRET, "replace": "X" }] }),
        )
    };
    assert!(edit("final_link").is_err());
    assert!(edit("ancestor_link/secret.txt").is_err());
    for path in [
        "final_link",
        "dangling_link",
        "ancestor_link/new.txt",
        "ancestor_link/deep/new.txt",
    ] {
        let r = fx.call("write_file", &json!({ "path": path, "content": "x" }));
        assert!(r.is_err(), "write_file {path}: {r:?}");
    }
    assert!(fx.secret_untouched());
    assert!(!fx.outside.path().join("new.txt").exists());
    assert!(!fx.outside.path().join("deep").exists());
    assert!(!fx.outside.path().join("missing.txt").exists());
}

#[test]
fn write_and_edit_still_work_inside_the_root() {
    let fx = Fixture::new();
    fx.call(
        "write_file",
        &json!({ "path": "new/dir/created.txt", "content": "hello\n" }),
    )
    .expect("create");
    assert!(fx
        .call(
            "write_file",
            &json!({ "path": "new/dir/created.txt", "content": "again" })
        )
        .is_err());
    fx.call(
        "edit_file",
        &json!({ "edits": [{ "path": "inside.txt", "search": "inside body", "replace": "edited" }] }),
    )
    .expect("edit");
    assert_eq!(read(fx.work.path(), "inside.txt"), "edited\n");
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).expect("read")
}

#[test]
fn unreadable_ancestor_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let locked = fx.work.path().join("locked");
    std::fs::create_dir(&locked).expect("locked");
    std::fs::write(locked.join("f.txt"), "x").expect("seed");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let readable = std::fs::read_dir(&locked).is_ok();
    let r = fx.call("read_file", &json!({ "path": "locked/f.txt" }));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("restore");
    if !readable {
        assert!(r.is_err(), "inaccessible path must fail closed: {r:?}");
    }
}
