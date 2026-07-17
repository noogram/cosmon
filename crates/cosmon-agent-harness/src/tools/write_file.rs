// SPDX-License-Identifier: AGPL-3.0-only

//! `write_file` — create a new file with the supplied contents, in
//! one shot, atomically.
//!
//! # Why a separate tool, not "edit_file with empty search"
//!
//! `edit_file` already supports a create-file mode (empty `search`,
//! full body in `replace`), but it carries the diff-grammar (a list of
//! [`crate::tools::edit_file::EditOp`]s) and the search/replace
//! ambiguity discipline. For the *create from scratch* use case — a
//! brand-new ADR draft, a fresh test fixture, a regenerated config —
//! the diff envelope is needless ceremony.
//!
//! # Discipline: create-only
//!
//! An earlier `write_file` was retired because it
//! invited the model to re-emit unchanged lines on existing files,
//! which is hallucination-prone on anything >200 lines. This
//! re-introduction is **strictly create-only**: the target must not
//! exist; if it does, the tool refuses with
//! [`crate::tool::ToolError::Io`] and points the model at `edit_file`.
//! There is no overwrite path. The load-bearing concern is preserved
//! by construction.
//!
//! # Atomic on-disk semantics
//!
//! Write goes through a sibling `tmp` file + `fs::rename`, the same
//! POSIX-atomic path `edit_file` uses. On success the target either has the
//! full new content or — on failure — does not exist at all.
//!
//! # Path safety
//!
//! [`crate::tool::sanitize_join`] rejects absolute paths and `..`
//! segments. A symlink-ancestor check refuses targets whose canonical
//! path (or any existing ancestor's canonical path) escapes
//! `work_dir`. Matches the discipline in `edit_file`.
//!
//! # Wire shape
//!
//! Arguments:
//!
//! ```json
//! { "path": "docs/new-note.md",
//!   "content": "# Title\n\nBody\n" }
//! ```
//!
//! Result:
//!
//! ```json
//! { "path": "docs/new-note.md", "bytes_written": 16 }
//! ```

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tool::{sanitize_join, ParametersSchema, Tool, ToolDeclaration, ToolError};

#[derive(Debug, Deserialize)]
struct WriteParams {
    path: String,
    content: String,
}

/// Payload returned by `write_file` on success.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    /// Path that was created, relative to `work_dir` (as supplied).
    pub path: String,
    /// Number of bytes written.
    pub bytes_written: u64,
}

/// `write_file` — whitelisted create-only file writer.
///
/// **Refuses if the target already exists** — see the module docs for
/// the create-only rationale.
#[derive(Debug, Default, Clone, Copy)]
pub struct WriteFile;

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "write_file",
            description: "Create a NEW file with the supplied contents inside the worker's \
                work_dir, atomically (tmp + rename). Refuses if the target already exists — \
                use edit_file (search-and-replace) to modify existing files. Use this for \
                fresh artifacts: new ADRs, new test fixtures, regenerated configs.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to work_dir. Must NOT already exist."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file contents, written verbatim."
                    }
                },
                "required": ["path", "content"]
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: WriteParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "write_file".to_owned(),
                message: e.to_string(),
            })?;
        let result = write(work_dir, &params)?;
        serde_json::to_string(&result).map_err(|e| ToolError::Io(e.to_string()))
    }
}

fn write(work_dir: &Path, params: &WriteParams) -> Result<WriteResult, ToolError> {
    let target = sanitize_join(work_dir, &params.path)?;
    ensure_inside_work_dir(work_dir, &target)?;

    // Create-only: refuse if the target already exists. `symlink_metadata`
    // does NOT follow links, so a symlink at `target` is reported as
    // existing (the ensure_inside_work_dir call above already refused it
    // when the link points outside, but we still refuse to clobber an
    // intra-worktree symlink for the same panel-verdict reason).
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(ToolError::Io(format!(
            "write_file refuses to overwrite existing target: {} (use edit_file to modify)",
            params.path
        )));
    }

    let bytes = params.content.as_bytes();
    write_via_rename(&target, bytes).map_err(|e| ToolError::Io(format!("{}: {e}", params.path)))?;

    let bytes_written = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    Ok(WriteResult {
        path: params.path.clone(),
        bytes_written,
    })
}

/// Atomic write through a sibling tmp file + `fs::rename`. Mirrors
/// `edit_file::write_via_rename` exactly — same nonce mix, same
/// rename atomicity guarantee. Kept as a private duplicate rather
/// than a `pub(crate)` reuse so the two tools stay independent at the
/// module boundary (subtractive design: cross-module helpers must
/// earn their keep, and two short functions are cheaper than the
/// coupling).
fn write_via_rename(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let nonce = {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let pid = u128::from(std::process::id());
        nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(pid)
    };
    let mut tmp_name: OsString = target.as_os_str().to_owned();
    tmp_name.push(format!(".{nonce:032x}.tmp"));
    let tmp_path = PathBuf::from(tmp_name);
    match std::fs::write(&tmp_path, bytes) {
        Ok(()) => {}
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    }
    match std::fs::rename(&tmp_path, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Reject targets whose canonical path (or any existing ancestor's
/// canonical path) escapes the canonical `work_dir`. Mirrors the
/// `edit_file` defense — see that module for the ssh-key clobber
/// scenario.
fn ensure_inside_work_dir(work_dir: &Path, target: &Path) -> Result<(), ToolError> {
    let canonical_work_dir = std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))?;

    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            return Err(ToolError::PathEscape(format!(
                "symlink target refused: {}",
                target.display()
            )));
        }
    }

    let mut probe = target.to_path_buf();
    loop {
        if probe.exists() {
            let canonical = std::fs::canonicalize(&probe)
                .map_err(|e| ToolError::Io(format!("canonicalize {}: {}", probe.display(), e)))?;
            if !canonical.starts_with(&canonical_work_dir) {
                return Err(ToolError::PathEscape(format!(
                    "path escapes work_dir via symlink: {}",
                    target.display()
                )));
            }
            return Ok(());
        }
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
            _ => {
                return Err(ToolError::PathEscape(format!(
                    "cannot resolve any ancestor of {}",
                    target.display()
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn run(dir: &Path, args: &serde_json::Value) -> Result<WriteResult, ToolError> {
        let raw = WriteFile.execute(&args.to_string(), dir)?;
        Ok(serde_json::from_str(&raw).expect("valid JSON"))
    }

    #[test]
    fn declaration_names_the_tool() {
        let decl = WriteFile.declaration();
        assert_eq!(decl.name, "write_file");
        let required = decl.parameters.as_json()["required"]
            .as_array()
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["path", "content"]);
    }

    #[test]
    fn creates_new_file() {
        let dir = tempdir().unwrap();
        let result = run(
            dir.path(),
            &serde_json::json!({
                "path": "out/note.md",
                "content": "# Hello"
            }),
        )
        .expect("write must succeed");
        assert_eq!(result.path, "out/note.md");
        assert_eq!(result.bytes_written, 7);
        let body = std::fs::read_to_string(dir.path().join("out/note.md")).expect("read back");
        assert_eq!(body, "# Hello");
    }

    #[test]
    fn refuses_to_overwrite_existing_file() {
        let dir = tempdir().unwrap();
        let existing = dir.path().join("existing.txt");
        std::fs::write(&existing, "old contents").expect("seed");
        let err = run(
            dir.path(),
            &serde_json::json!({
                "path": "existing.txt",
                "content": "new contents"
            }),
        )
        .expect_err("must refuse");
        assert!(matches!(err, ToolError::Io(_)), "got {err:?}");
        // Disk untouched.
        let body = std::fs::read_to_string(&existing).expect("read back");
        assert_eq!(body, "old contents");
    }

    #[test]
    fn creates_nested_parent_directories() {
        let dir = tempdir().unwrap();
        let result = run(
            dir.path(),
            &serde_json::json!({
                "path": "a/b/c/deep.txt",
                "content": "x"
            }),
        )
        .expect("write must succeed");
        assert_eq!(result.bytes_written, 1);
        let body = std::fs::read_to_string(dir.path().join("a/b/c/deep.txt")).expect("read back");
        assert_eq!(body, "x");
    }

    #[test]
    fn refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let err = run(
            dir.path(),
            &serde_json::json!({
                "path": "/etc/passwd",
                "content": "owned"
            }),
        )
        .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let err = run(
            dir.path(),
            &serde_json::json!({
                "path": "../outside.txt",
                "content": "owned"
            }),
        )
        .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn refuses_symlink_target_outside_work_dir() {
        let work = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "do not overwrite").expect("seed");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, work.path().join("link")).expect("symlink");

        #[cfg(unix)]
        {
            let err = run(
                work.path(),
                &serde_json::json!({
                    "path": "link",
                    "content": "owned"
                }),
            )
            .expect_err("must refuse symlink");
            assert!(matches!(err, ToolError::PathEscape(_)));
            let after = std::fs::read_to_string(&secret).expect("read");
            assert_eq!(after, "do not overwrite");
        }
    }

    #[test]
    fn no_tmp_leftovers_after_success() {
        let dir = tempdir().unwrap();
        run(
            dir.path(),
            &serde_json::json!({ "path": "a.txt", "content": "x" }),
        )
        .expect("write");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let leftovers: Vec<_> = entries.iter().filter(|n| n.contains(".tmp")).collect();
        assert!(
            leftovers.is_empty(),
            "atomic rename must leave no .tmp siblings, got {entries:?}"
        );
    }

    #[test]
    fn empty_content_creates_empty_file() {
        let dir = tempdir().unwrap();
        let result = run(
            dir.path(),
            &serde_json::json!({ "path": "empty.txt", "content": "" }),
        )
        .expect("write");
        assert_eq!(result.bytes_written, 0);
        assert!(dir.path().join("empty.txt").exists());
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let dir = tempdir().unwrap();
        let err = WriteFile
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
