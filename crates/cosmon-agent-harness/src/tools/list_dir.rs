// SPDX-License-Identifier: AGPL-3.0-only

//! `list_dir` — list entries of a directory inside the worker's
//! `work_dir`. Pairs with `read_file` / `grep` / `find_file` to give
//! the model a usable local-research surface.
//!
//! # Discipline
//!
//! - Path safety is enforced by [`crate::tool::sanitize_join`] —
//!   absolute paths and `..` segments are loud failures via
//!   [`crate::tool::ToolError::PathEscape`], same as every other
//!   tool in the registry.
//! - Symlink targets that escape `work_dir` are refused at walk time
//!   (defense in depth — the walker only follows links inside the
//!   work tree).
//! - Output is capped at [`MAX_ENTRIES`] entries with a `truncated`
//!   flag so a `list_dir` on `~/galaxies/knowledge` cannot saturate
//!   the model's context silently.
//! - Hidden entries (`.git`, `.cargo`, …) and the usual heavy build
//!   directories (`target`, `node_modules`, `.cosmon/state`) are
//!   skipped by default; the model can opt back in with
//!   `include_hidden=true` and `ignore_vcs=false`.
//!
//! # Wire shape
//!
//! Arguments:
//!
//! ```json
//! { "path": "src",
//!   "recursive": true,
//!   "max_depth": 3,
//!   "include_hidden": false,
//!   "ignore_vcs": true }
//! ```
//!
//! Result (JSON, single object):
//!
//! ```json
//! { "entries": [
//!     { "path": "src/lib.rs", "type": "file", "size": 1234 },
//!     { "path": "src/tools", "type": "dir",  "size": null }
//!   ],
//!   "truncated": false,
//!   "total": 2 }
//! ```
//!
//! `path` in each entry is relative to `work_dir` (forward-slash
//! delimited for cross-platform readability when the harness is later
//! ported off the Unix-only `exec_command` perimeter).

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::tool::{sanitize_join, ParametersSchema, Tool, ToolDeclaration, ToolError};

/// Cap on the number of entries returned by a single `list_dir` call.
/// Mirrors the truncation discipline of `read_file` /
/// `exec_command` — better to surface the cap loudly than silently
/// blow the model's context (forgemaster §AH4 reasoning).
pub const MAX_ENTRIES: usize = 2_000;

/// Hard ceiling on the recursive walk depth — prevents accidental
/// scans of huge worktrees when the caller forgets to set
/// `max_depth`.
pub const DEFAULT_MAX_DEPTH: usize = 16;

#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    max_depth: Option<usize>,
    #[serde(default)]
    include_hidden: bool,
    #[serde(default = "default_true")]
    ignore_vcs: bool,
}

fn default_path() -> String {
    ".".to_owned()
}
fn default_true() -> bool {
    true
}

/// Per-entry record returned by `list_dir`.
///
/// `#[non_exhaustive]` (tolnay F2) — additive fields (e.g. mtime,
/// permissions) must not require a major bump.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListEntry {
    /// Path relative to `work_dir`, forward-slash delimited.
    pub path: String,
    /// `"file"`, `"dir"`, or `"symlink"`.
    pub kind: String,
    /// File size in bytes; `None` for directories or unreadable
    /// metadata.
    pub size: Option<u64>,
}

/// Payload returned by `list_dir`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResult {
    /// Listed entries, sorted by relative path (BTreeMap-style stable
    /// order — same prefix-cache discipline as [`crate::ToolRegistry`]).
    pub entries: Vec<ListEntry>,
    /// `true` if the walk was cut short by [`MAX_ENTRIES`]. The model
    /// then knows to narrow the next call with `path` or
    /// `max_depth`.
    pub truncated: bool,
    /// Number of entries returned (equal to `entries.len()`). Surfaced
    /// alongside `truncated` so the model does not have to count.
    pub total: usize,
}

/// `list_dir` — whitelisted directory-listing capability.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListDir;

impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "list_dir",
            description: "List entries of a directory inside the worker's work_dir. \
                Defaults to non-recursive listing of '.'; set recursive=true and an optional \
                max_depth to walk the tree. Hidden entries and VCS / build directories \
                (.git, target, node_modules) are skipped by default — set include_hidden=true \
                or ignore_vcs=false to opt back in. Output is capped at 2000 entries with \
                a `truncated` flag.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to work_dir. Defaults to '.'."
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "Walk subdirectories. Defaults to false."
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum recursion depth (only when recursive=true). \
                            Defaults to 16."
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Include dotfiles and dot-directories. Defaults to false."
                    },
                    "ignore_vcs": {
                        "type": "boolean",
                        "description": "Respect .gitignore and skip VCS directories. \
                            Defaults to true."
                    }
                }
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: ListParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "list_dir".to_owned(),
                message: e.to_string(),
            })?;
        let result = list(work_dir, &params)?;
        serde_json::to_string(&result).map_err(|e| ToolError::Io(e.to_string()))
    }
}

fn list(work_dir: &Path, params: &ListParams) -> Result<ListResult, ToolError> {
    let target = sanitize_join(work_dir, &params.path)?;
    if !target.exists() {
        return Err(ToolError::Io(format!(
            "list_dir target does not exist: {}",
            params.path
        )));
    }
    if !target.is_dir() {
        return Err(ToolError::Io(format!(
            "list_dir target is not a directory: {}",
            params.path
        )));
    }

    let canonical_work_dir = std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))?;

    let depth = if params.recursive {
        params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH)
    } else {
        1
    };

    let mut builder = WalkBuilder::new(&target);
    builder
        .max_depth(Some(depth))
        .standard_filters(params.ignore_vcs)
        .hidden(!params.include_hidden)
        .ignore(params.ignore_vcs)
        .git_ignore(params.ignore_vcs)
        .git_exclude(params.ignore_vcs)
        .git_global(params.ignore_vcs)
        // Never follow symlinks — escape-via-link is a worktree breach,
        // not a feature.
        .follow_links(false);

    // Default-skipped heavy directories beyond `.gitignore`. Even if
    // the worktree has no `.gitignore`, `target/` and `node_modules/`
    // are too noisy to belong in a `list_dir` result by default.
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        !matches!(name.as_str(), "target" | "node_modules")
    });

    let mut entries: Vec<ListEntry> = Vec::new();
    let mut truncated = false;
    for result in builder.build() {
        let Ok(entry) = result else { continue };

        let path = entry.path();
        // Skip the root itself — caller already named it.
        if path == target.as_path() {
            continue;
        }

        // Confirm the entry is inside the canonical work_dir. The
        // walker is configured with follow_links=false so this is
        // belt-and-suspenders, but a future contributor flipping the
        // switch should not silently leak entries outside.
        if let Ok(canonical) = std::fs::canonicalize(path) {
            if !canonical.starts_with(&canonical_work_dir) {
                continue;
            }
        }

        let rel = path.strip_prefix(work_dir).unwrap_or(path);
        let rel_str = path_to_forward_slash(rel);

        let file_type = entry.file_type();
        let (kind, size) = match file_type {
            Some(ft) if ft.is_symlink() => ("symlink", None),
            Some(ft) if ft.is_dir() => ("dir", None),
            Some(_) => {
                let size = entry.metadata().ok().map(|m| m.len());
                ("file", size)
            }
            None => continue,
        };

        entries.push(ListEntry {
            path: rel_str,
            kind: kind.to_owned(),
            size,
        });

        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let total = entries.len();
    Ok(ListResult {
        entries,
        truncated,
        total,
    })
}

/// Render a relative path with `/` separators, regardless of host
/// platform. The harness is Unix-only at write time, but normalising
/// the wire shape now keeps the JSON contract stable when the worker
/// substrate ports to Windows later.
fn path_to_forward_slash(path: &Path) -> String {
    let mut s = String::new();
    let mut first = true;
    for component in path.components() {
        let part: PathBuf = component.as_os_str().into();
        if !first {
            s.push('/');
        }
        s.push_str(&part.to_string_lossy());
        first = false;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("seed parent");
        }
        std::fs::write(&p, content).expect("seed write");
    }

    fn run(dir: &Path, args: &serde_json::Value) -> ListResult {
        let raw = ListDir
            .execute(&args.to_string(), dir)
            .expect("list_dir must succeed");
        serde_json::from_str(&raw).expect("valid JSON")
    }

    #[test]
    fn declaration_names_the_tool() {
        let decl = ListDir.declaration();
        assert_eq!(decl.name, "list_dir");
        assert!(decl.parameters.as_json()["properties"]["path"].is_object());
    }

    #[test]
    fn lists_top_level_entries_non_recursive() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "a.txt", "hello");
        seed(dir.path(), "sub/b.txt", "world");
        let result = run(dir.path(), &serde_json::json!({}));
        let names: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        // Non-recursive: only top-level a.txt and sub/, not sub/b.txt.
        assert!(names.contains(&"a.txt"), "missing a.txt: {names:?}");
        assert!(names.contains(&"sub"), "missing sub: {names:?}");
        assert!(!names.contains(&"sub/b.txt"));
    }

    #[test]
    fn recursive_walks_subtree() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "a.txt", "x");
        seed(dir.path(), "sub/deep/b.txt", "y");
        let result = run(
            dir.path(),
            &serde_json::json!({ "recursive": true, "max_depth": 5 }),
        );
        let paths: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"sub/deep/b.txt"));
    }

    #[test]
    fn entry_kinds_are_correct() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "file.txt", "hello world");
        std::fs::create_dir_all(dir.path().join("empty_dir")).unwrap();
        let result = run(dir.path(), &serde_json::json!({}));
        for entry in &result.entries {
            match entry.path.as_str() {
                "file.txt" => {
                    assert_eq!(entry.kind, "file");
                    assert_eq!(entry.size, Some(11));
                }
                "empty_dir" => {
                    assert_eq!(entry.kind, "dir");
                    assert!(entry.size.is_none());
                }
                _ => {}
            }
        }
    }

    #[test]
    fn ignores_target_and_node_modules_by_default() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/main.rs", "fn main() {}");
        seed(dir.path(), "target/debug/main", "bin");
        seed(dir.path(), "node_modules/foo/index.js", "js");
        let result = run(dir.path(), &serde_json::json!({ "recursive": true }));
        let paths: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(
            !paths.iter().any(|p| p.starts_with("target")),
            "target/ should be ignored: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("node_modules")),
            "node_modules/ should be ignored: {paths:?}"
        );
    }

    #[test]
    fn hides_dotfiles_by_default() {
        let dir = tempdir().unwrap();
        seed(dir.path(), ".env", "SECRET=1");
        seed(dir.path(), "visible.txt", "x");
        let result = run(dir.path(), &serde_json::json!({}));
        let paths: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"visible.txt"));
        assert!(!paths.contains(&".env"));
    }

    #[test]
    fn include_hidden_surfaces_dotfiles() {
        let dir = tempdir().unwrap();
        seed(dir.path(), ".env", "SECRET=1");
        let result = run(dir.path(), &serde_json::json!({ "include_hidden": true }));
        let paths: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&".env"), "got {paths:?}");
    }

    #[test]
    fn refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let err = ListDir
            .execute(
                &serde_json::json!({ "path": "/etc" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let err = ListDir
            .execute(
                &serde_json::json!({ "path": "../escape" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn missing_target_is_io_error() {
        let dir = tempdir().unwrap();
        let err = ListDir
            .execute(
                &serde_json::json!({ "path": "nope" }).to_string(),
                dir.path(),
            )
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn not_a_directory_is_io_error() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "file.txt", "x");
        let err = ListDir
            .execute(
                &serde_json::json!({ "path": "file.txt" }).to_string(),
                dir.path(),
            )
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn truncates_when_over_max_entries() {
        let dir = tempdir().unwrap();
        // Seed > MAX_ENTRIES files so the walker hits the cap.
        for i in 0..(MAX_ENTRIES + 50) {
            seed(dir.path(), &format!("f{i:05}.txt"), "x");
        }
        let result = run(dir.path(), &serde_json::json!({}));
        assert!(result.truncated);
        assert_eq!(result.total, MAX_ENTRIES);
    }

    #[test]
    fn entries_are_sorted_for_stable_output() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "zebra.txt", "z");
        seed(dir.path(), "alpha.txt", "a");
        seed(dir.path(), "mango.txt", "m");
        let result = run(dir.path(), &serde_json::json!({}));
        let paths: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let dir = tempdir().unwrap();
        let err = ListDir
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
