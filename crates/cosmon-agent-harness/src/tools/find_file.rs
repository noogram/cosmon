// SPDX-License-Identifier: AGPL-3.0-only

//! `find_file` — locate files by name (gitignore-style glob) inside
//! the worker's `work_dir`. Complements `grep` (which searches file
//! *contents*) and `list_dir` (which enumerates a single directory).
//!
//! # Discipline
//!
//! - Path safety via [`crate::tool::sanitize_join`].
//! - Walk respects `.gitignore`, skips dotfiles and heavy build
//!   directories by default (`target`, `node_modules`).
//! - Globs are the gitignore dialect — `**/` for any number of
//!   directories, `*` for one path component, `?` for one byte.
//!   Patterns are matched against the path *relative to the search
//!   root*, so `*.rs` matches files in subdirectories the same way
//!   `git ls-files '*.rs'` does.
//! - Output is capped at [`MAX_MATCHES`] paths with a `truncated`
//!   flag (same contract as `read_file` / `grep` / `list_dir`).
//!
//! # Wire shape
//!
//! Arguments:
//!
//! ```json
//! { "pattern": "**/*.rs",
//!   "path": ".",
//!   "max_results": 200,
//!   "include_hidden": false,
//!   "ignore_vcs": true }
//! ```
//!
//! Result (JSON, single object):
//!
//! ```json
//! { "matches": ["src/lib.rs", "src/tools/mod.rs"],
//!   "truncated": false,
//!   "total": 2 }
//! ```

use std::path::Path;

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::tool::{sanitize_join, ParametersSchema, Tool, ToolDeclaration, ToolError};

/// Hard cap on the number of paths returned by a single `find_file`
/// call. Matches the truncation discipline of every other read-shaped
/// tool in the registry.
pub const MAX_MATCHES: usize = 2_000;

#[derive(Debug, Deserialize)]
struct FindParams {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    max_results: Option<usize>,
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

/// Payload returned by `find_file`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResult {
    /// Matched paths relative to `work_dir`, forward-slash delimited
    /// and sorted lexicographically (stable iteration order — same
    /// prefix-cache discipline as the rest of the registry).
    pub matches: Vec<String>,
    /// `true` when the search was cut short by `max_results`.
    pub truncated: bool,
    /// Number of paths returned (equal to `matches.len()`).
    pub total: usize,
}

/// `find_file` — whitelisted name-glob search.
#[derive(Debug, Default, Clone, Copy)]
pub struct FindFile;

impl Tool for FindFile {
    fn name(&self) -> &'static str {
        "find_file"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "find_file",
            description: "Locate files by name (gitignore-style glob) inside the worker's \
                work_dir. Walks the tree respecting .gitignore and skipping heavy directories \
                (target, node_modules, .git). Globs use the gitignore dialect — `**/` for any \
                number of directories, `*` for one path component. Returns a sorted list of \
                paths relative to work_dir; capped at 2000 matches with a `truncated` flag.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Gitignore-style glob (e.g. `**/*.rs`, `src/*.toml`)."
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to work_dir. Defaults to '.'."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Cap on returned paths. Default and ceiling: 2000."
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Search hidden (.*) files. Default false."
                    },
                    "ignore_vcs": {
                        "type": "boolean",
                        "description": "Respect .gitignore. Default true."
                    }
                },
                "required": ["pattern"]
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: FindParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "find_file".to_owned(),
                message: e.to_string(),
            })?;
        let result = find(work_dir, &params)?;
        serde_json::to_string(&result).map_err(|e| ToolError::Io(e.to_string()))
    }
}

fn find(work_dir: &Path, params: &FindParams) -> Result<FindResult, ToolError> {
    let root = sanitize_join(work_dir, &params.path)?;
    if !root.exists() {
        return Err(ToolError::Io(format!(
            "find_file root does not exist: {}",
            params.path
        )));
    }

    let cap = params
        .max_results
        .map_or(MAX_MATCHES, |n| n.clamp(1, MAX_MATCHES));

    let canonical_work_dir = std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))?;

    // The user's pattern is applied as a POST-WALK filter, not as an
    // `ignore::overrides::Override`. The Override engine treats every
    // user glob as a whitelist that *overrides* gitignore — a
    // gitignored file matching `*.rs` would then surface, which is
    // not the contract `find_file` advertises. The walker handles
    // gitignore / VCS / heavy-dir skipping; this matcher decides
    // which of the SURVIVING files match the user's name pattern.
    let matcher = build_matcher(&params.pattern)?;

    let mut builder = WalkBuilder::new(&root);
    builder
        .standard_filters(params.ignore_vcs)
        .hidden(!params.include_hidden)
        .ignore(params.ignore_vcs)
        .git_ignore(params.ignore_vcs)
        .git_exclude(params.ignore_vcs)
        .git_global(params.ignore_vcs)
        .follow_links(false);

    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        !matches!(name.as_str(), "target" | "node_modules")
    });

    let mut results: Vec<String> = Vec::new();
    let mut truncated = false;

    for walked in builder.build() {
        let Ok(entry) = walked else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();

        if let Ok(canonical) = std::fs::canonicalize(path) {
            if !canonical.starts_with(&canonical_work_dir) {
                continue;
            }
        }

        // Match the user's glob against the path relative to the search
        // root (not relative to work_dir) — that mirrors how
        // `git ls-files <glob>` behaves: a pattern like `**/*.rs` is
        // anchored at the search root, not at the worktree root.
        let rel_to_root = path.strip_prefix(&root).unwrap_or(path);
        if !matcher.is_match(rel_to_root) {
            continue;
        }

        let rel_to_workdir = path.strip_prefix(work_dir).unwrap_or(path);
        results.push(path_to_forward_slash(rel_to_workdir));

        if results.len() >= cap {
            truncated = true;
            break;
        }
    }

    results.sort();
    let total = results.len();
    Ok(FindResult {
        matches: results,
        truncated,
        total,
    })
}

/// Build a globset matcher that accepts two natural gitignore-style
/// forms:
///
/// - A pattern with a `/` (or starting with `**/`) matches against
///   the full relative path, e.g. `src/*.rs` matches `src/lib.rs`
///   but not `tests/it.rs`.
/// - A pattern with no `/` matches against the basename of any file
///   under the tree, e.g. `Cargo.toml` matches `Cargo.toml` at the
///   root and `crates/foo/Cargo.toml` anywhere below. This mirrors
///   gitignore's "no-slash-anywhere" convention and matches the
///   shape `find <root> -name <pattern>` users already know.
fn build_matcher(pattern: &str) -> Result<PathMatcher, ToolError> {
    let raw = Glob::new(pattern)
        .map_err(|e| ToolError::InvalidArguments {
            tool: "find_file".to_owned(),
            message: format!("invalid pattern {pattern:?}: {e}"),
        })?
        .compile_matcher();
    let has_slash = pattern.contains('/');
    let basename_only_matcher: Option<GlobMatcher> = if has_slash {
        None
    } else {
        // Anchor the no-slash form to the basename via a `**/` prefix
        // so the same matcher reaches files at any depth.
        Some(
            Glob::new(&format!("**/{pattern}"))
                .map_err(|e| ToolError::InvalidArguments {
                    tool: "find_file".to_owned(),
                    message: format!("invalid pattern {pattern:?}: {e}"),
                })?
                .compile_matcher(),
        )
    };
    Ok(PathMatcher {
        raw,
        basename_only_matcher,
    })
}

struct PathMatcher {
    raw: GlobMatcher,
    basename_only_matcher: Option<GlobMatcher>,
}

impl PathMatcher {
    fn is_match(&self, path: &Path) -> bool {
        if self.raw.is_match(path) {
            return true;
        }
        if let Some(matcher) = &self.basename_only_matcher {
            return matcher.is_match(path);
        }
        false
    }
}

/// Render a relative path with `/` separators (see
/// `list_dir::path_to_forward_slash`'s rationale).
fn path_to_forward_slash(path: &Path) -> String {
    let mut s = String::new();
    let mut first = true;
    for component in path.components() {
        let part = component.as_os_str().to_string_lossy().into_owned();
        if !first {
            s.push('/');
        }
        s.push_str(&part);
        first = false;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed(dir: &Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("seed parent");
        }
        std::fs::write(&p, content).expect("seed write");
    }

    fn run(dir: &Path, args: &serde_json::Value) -> FindResult {
        let raw = FindFile
            .execute(&args.to_string(), dir)
            .expect("find_file must succeed");
        serde_json::from_str(&raw).expect("valid JSON")
    }

    #[test]
    fn declaration_names_the_tool() {
        let decl = FindFile.declaration();
        assert_eq!(decl.name, "find_file");
        let required = decl.parameters.as_json()["required"]
            .as_array()
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }

    #[test]
    fn finds_files_by_extension_glob() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/lib.rs", b"x");
        seed(dir.path(), "src/tools/mod.rs", b"x");
        seed(dir.path(), "README.md", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "**/*.rs" }));
        let paths: Vec<&str> = result.matches.iter().map(String::as_str).collect();
        assert_eq!(paths, vec!["src/lib.rs", "src/tools/mod.rs"]);
    }

    #[test]
    fn finds_by_exact_name() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/lib.rs", b"x");
        seed(dir.path(), "Cargo.toml", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "Cargo.toml" }));
        assert_eq!(result.matches, vec!["Cargo.toml"]);
    }

    #[test]
    fn finds_with_directory_glob() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/main.rs", b"x");
        seed(dir.path(), "src/lib.rs", b"x");
        seed(dir.path(), "tests/it.rs", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "src/*.rs" }));
        let paths: Vec<&str> = result.matches.iter().map(String::as_str).collect();
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn ignores_heavy_dirs_by_default() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/lib.rs", b"x");
        seed(dir.path(), "target/debug/build/foo.rs", b"x");
        seed(dir.path(), "node_modules/pkg/index.rs", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "**/*.rs" }));
        let paths: Vec<&str> = result.matches.iter().map(String::as_str).collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempdir().unwrap();
        seed(dir.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        seed(dir.path(), ".gitignore", b"hidden.rs\n");
        seed(dir.path(), "hidden.rs", b"x");
        seed(dir.path(), "visible.rs", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "*.rs" }));
        let paths: Vec<&str> = result.matches.iter().map(String::as_str).collect();
        assert!(paths.contains(&"visible.rs"));
        assert!(!paths.contains(&"hidden.rs"));
    }

    #[test]
    fn truncates_at_max_results() {
        let dir = tempdir().unwrap();
        for i in 0..(MAX_MATCHES + 50) {
            seed(dir.path(), &format!("f{i:05}.txt"), b"x");
        }
        let result = run(dir.path(), &serde_json::json!({ "pattern": "*.txt" }));
        assert!(result.truncated);
        assert_eq!(result.total, MAX_MATCHES);
    }

    #[test]
    fn caller_max_results_is_honored() {
        let dir = tempdir().unwrap();
        for i in 0..20 {
            seed(dir.path(), &format!("f{i:02}.txt"), b"x");
        }
        let result = run(
            dir.path(),
            &serde_json::json!({ "pattern": "*.txt", "max_results": 5 }),
        );
        assert!(result.truncated);
        assert_eq!(result.total, 5);
    }

    #[test]
    fn matches_are_sorted() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "zebra.rs", b"x");
        seed(dir.path(), "alpha.rs", b"x");
        seed(dir.path(), "mango.rs", b"x");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "*.rs" }));
        assert_eq!(result.matches, vec!["alpha.rs", "mango.rs", "zebra.rs"]);
    }

    #[test]
    fn refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let err = FindFile
            .execute(
                &serde_json::json!({ "pattern": "*.rs", "path": "/etc" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let err = FindFile
            .execute(
                &serde_json::json!({ "pattern": "*.rs", "path": "../escape" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn invalid_pattern_is_loud_invalid_arguments() {
        let dir = tempdir().unwrap();
        // `[` opens a character class; ignore's globset rejects an
        // unterminated class.
        let err = FindFile
            .execute(
                &serde_json::json!({ "pattern": "[a-z" }).to_string(),
                dir.path(),
            )
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn missing_root_is_io_error() {
        let dir = tempdir().unwrap();
        let err = FindFile
            .execute(
                &serde_json::json!({ "pattern": "*", "path": "missing" }).to_string(),
                dir.path(),
            )
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let dir = tempdir().unwrap();
        let err = FindFile
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
