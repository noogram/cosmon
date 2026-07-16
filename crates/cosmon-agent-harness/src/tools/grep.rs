// SPDX-License-Identifier: AGPL-3.0-only

//! `grep` — regex search over text files inside the worker's
//! `work_dir`. The local-research counterpart to `read_file`: rather
//! than the model spending an `exec_command` turn on `rg` (and burning
//! both a process spawn and the 32 KiB output cap to filter the
//! result), the harness walks the worktree in-process and returns
//! structured matches.
//!
//! # Discipline
//!
//! - Path safety via [`crate::tool::sanitize_join`] — refused
//!   absolute / parent-escape paths surface as
//!   [`crate::tool::ToolError::PathEscape`].
//! - Walk respects `.gitignore`, skips dotfiles and the canonical
//!   heavy directories (`.git`, `target`, `node_modules`) unless the
//!   caller flips `include_hidden` / `ignore_vcs`.
//! - Binary files are detected by a null-byte sniff on the first 8 KiB
//!   and skipped silently (the model asked for *text* matches).
//! - Per-line decoding uses `String::from_utf8_lossy`, so binary noise
//!   inside an otherwise-UTF-8 file does not crash the search; the
//!   `line` field is always valid UTF-8.
//! - Output is hard-capped at [`MAX_MATCHES`] hits with a `truncated`
//!   flag — same loud-truncation contract as `read_file` and
//!   `exec_command`.
//!
//! # Wire shape
//!
//! Arguments:
//!
//! ```json
//! { "pattern": "fn\\s+main",
//!   "path": "src",
//!   "fixed_string": false,
//!   "case_insensitive": false,
//!   "include_glob": ["**/*.rs"],
//!   "exclude_glob": ["**/tests/**"],
//!   "max_results": 200,
//!   "include_hidden": false,
//!   "ignore_vcs": true }
//! ```
//!
//! Result (JSON, single object):
//!
//! ```json
//! { "matches": [
//!     { "path": "src/lib.rs", "line_number": 42, "line": "fn main() {" }
//!   ],
//!   "truncated": false,
//!   "total": 1,
//!   "files_scanned": 12 }
//! ```

use std::io::{BufRead, BufReader};
use std::path::Path;

use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::tool::{sanitize_join, ParametersSchema, Tool, ToolDeclaration, ToolError};

/// Hard cap on matches returned by a single `grep` call. Mirrors the
/// truncation discipline of `read_file` / `exec_command`.
pub const MAX_MATCHES: usize = 1_000;

/// Maximum line length surfaced in [`GrepMatch::line`]. Lines longer
/// than this are truncated with a `...` suffix so a binary-looking
/// match in a large generated file cannot saturate the model's
/// context.
pub const MAX_LINE_LENGTH: usize = 512;

/// First `BINARY_SNIFF_BYTES` of each file are scanned for a NUL byte
/// to decide whether to skip a binary file silently.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

// `struct_excessive_bools` — every flag here is a user-facing toggle
// (fixed_string, case_insensitive, include_hidden, ignore_vcs); collapsing
// them into a bit-field enum hurts the wire shape the model sees more
// than it helps the type. The four booleans ARE the API.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize)]
struct GrepParams {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    fixed_string: bool,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    include_glob: Vec<String>,
    #[serde(default)]
    exclude_glob: Vec<String>,
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

/// One match returned by `grep`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    /// File path relative to `work_dir`, forward-slash delimited.
    pub path: String,
    /// 1-indexed line number of the match.
    pub line_number: usize,
    /// Matching line, decoded lossily as UTF-8 and truncated to
    /// [`MAX_LINE_LENGTH`] bytes with a `...` suffix when oversize.
    /// Trailing `\r` / `\n` are stripped.
    pub line: String,
}

/// Payload returned by `grep`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepResult {
    /// Matches in walk order. `BTreeMap`-style stable iteration is
    /// approximated by ripgrep's own ordering (which walks
    /// `read_dir` results sorted via the `ignore` crate's standard
    /// configuration).
    pub matches: Vec<GrepMatch>,
    /// `true` when the search was cut short by `max_results`.
    pub truncated: bool,
    /// Number of matches returned (equal to `matches.len()`).
    pub total: usize,
    /// Number of files actually opened and scanned (skipped binaries
    /// don't count).
    pub files_scanned: usize,
}

/// `grep` — whitelisted in-process regex search.
#[derive(Debug, Default, Clone, Copy)]
pub struct Grep;

impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "grep",
            description: "Regex search across files inside the worker's work_dir. \
                Walks the tree respecting .gitignore and skipping heavy directories \
                (target, node_modules, .git). Returns one match record per matching line \
                (1-indexed). Lines longer than 512 bytes are truncated with a '...' suffix; \
                binary files are skipped. Output is capped at 1000 matches by default; the \
                `truncated` flag fires when the cap binds. Use `fixed_string=true` to disable \
                regex parsing; `case_insensitive=true` for ASCII case folding; \
                `include_glob` / `exclude_glob` to filter files by gitignore-style patterns.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern. Regex by default; \
                            set fixed_string=true for literal matching."
                    },
                    "path": {
                        "type": "string",
                        "description": "Path relative to work_dir. Defaults to '.'."
                    },
                    "fixed_string": {
                        "type": "boolean",
                        "description": "Treat `pattern` as a literal string. Default false."
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Case-insensitive match. Default false."
                    },
                    "include_glob": {
                        "type": "array",
                        "description": "Gitignore-style globs; only matching files are searched.",
                        "items": {"type": "string"}
                    },
                    "exclude_glob": {
                        "type": "array",
                        "description": "Gitignore-style globs; matching files are skipped.",
                        "items": {"type": "string"}
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Cap on returned matches. Default and ceiling: 1000."
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
        let params: GrepParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "grep".to_owned(),
                message: e.to_string(),
            })?;
        let result = grep(work_dir, &params)?;
        serde_json::to_string(&result).map_err(|e| ToolError::Io(e.to_string()))
    }
}

// `too_many_lines` — the grep loop is one cohesive scan: regex compile,
// override build, walker setup, per-file binary sniff, per-line match.
// Splitting it just to shrink the body would either tear an iterator
// into multiple borrows of `regex` / `cap` / `canonical_work_dir`, or
// pass them through helper functions that exist only to dodge the lint.
// The shape is intentional.
#[allow(clippy::too_many_lines)]
fn grep(work_dir: &Path, params: &GrepParams) -> Result<GrepResult, ToolError> {
    let root = sanitize_join(work_dir, &params.path)?;
    if !root.exists() {
        return Err(ToolError::Io(format!(
            "grep root does not exist: {}",
            params.path
        )));
    }

    let pattern = if params.fixed_string {
        regex::escape(&params.pattern)
    } else {
        params.pattern.clone()
    };
    let regex: Regex = RegexBuilder::new(&pattern)
        .case_insensitive(params.case_insensitive)
        .build()
        .map_err(|e| ToolError::InvalidArguments {
            tool: "grep".to_owned(),
            message: format!("invalid regex: {e}"),
        })?;

    let cap = params
        .max_results
        .map_or(MAX_MATCHES, |n| n.clamp(1, MAX_MATCHES));

    let canonical_work_dir = std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))?;

    // Build gitignore-style overrides for include/exclude. `Override`
    // semantics: a whitelist glob makes everything else implicitly
    // ignored; a `!` prefix turns it back to an explicit ignore. We
    // model `include_glob` as whitelists and `exclude_glob` as ignores
    // (prefixed with `!` in the override engine's vocabulary).
    let mut over = OverrideBuilder::new(&root);
    for include in &params.include_glob {
        over.add(include).map_err(|e| ToolError::InvalidArguments {
            tool: "grep".to_owned(),
            message: format!("invalid include_glob {include:?}: {e}"),
        })?;
    }
    for exclude in &params.exclude_glob {
        let inverted = format!("!{exclude}");
        over.add(&inverted)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "grep".to_owned(),
                message: format!("invalid exclude_glob {exclude:?}: {e}"),
            })?;
    }
    let overrides = over.build().map_err(|e| ToolError::InvalidArguments {
        tool: "grep".to_owned(),
        message: format!("build overrides: {e}"),
    })?;

    let mut builder = WalkBuilder::new(&root);
    builder
        .standard_filters(params.ignore_vcs)
        .hidden(!params.include_hidden)
        .ignore(params.ignore_vcs)
        .git_ignore(params.ignore_vcs)
        .git_exclude(params.ignore_vcs)
        .git_global(params.ignore_vcs)
        .overrides(overrides)
        .follow_links(false);

    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy().to_string();
        !matches!(name.as_str(), "target" | "node_modules")
    });

    let mut hits: Vec<GrepMatch> = Vec::new();
    let mut files_scanned = 0_usize;
    let mut truncated = false;

    'outer: for walked in builder.build() {
        let Ok(entry) = walked else { continue };
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }

        // Defense in depth: refuse to even open a file whose canonical
        // path escapes the canonical work_dir.
        if let Ok(canonical) = std::fs::canonicalize(path) {
            if !canonical.starts_with(&canonical_work_dir) {
                continue;
            }
        }

        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };

        // Sniff the first chunk for NUL bytes — binary files are
        // skipped silently. `BufReader::fill_buf` borrows from the
        // internal buffer without advancing the cursor, so the
        // subsequent line iteration sees the entire file.
        let mut reader = BufReader::with_capacity(BINARY_SNIFF_BYTES, file);
        match reader.fill_buf() {
            Ok(buf) => {
                if buf.contains(&0) {
                    continue;
                }
            }
            Err(_) => continue,
        }

        files_scanned += 1;
        let rel = path.strip_prefix(work_dir).unwrap_or(path);
        let rel_str = path_to_forward_slash(rel);

        let mut line_no: usize = 0;
        let mut buf: Vec<u8> = Vec::with_capacity(256);
        loop {
            buf.clear();
            let Ok(read) = reader.read_until(b'\n', &mut buf) else {
                break;
            };
            if read == 0 {
                break;
            }
            line_no += 1;
            // Strip the trailing newline(s).
            while matches!(buf.last(), Some(&b'\n' | &b'\r')) {
                buf.pop();
            }
            let line = String::from_utf8_lossy(&buf);
            if regex.is_match(&line) {
                let truncated_line = truncate_line(line.into_owned());
                hits.push(GrepMatch {
                    path: rel_str.clone(),
                    line_number: line_no,
                    line: truncated_line,
                });
                if hits.len() >= cap {
                    truncated = true;
                    break 'outer;
                }
            }
        }
    }

    let total = hits.len();
    Ok(GrepResult {
        matches: hits,
        truncated,
        total,
        files_scanned,
    })
}

fn truncate_line(mut s: String) -> String {
    if s.len() <= MAX_LINE_LENGTH {
        return s;
    }
    let mut cut = MAX_LINE_LENGTH;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("...");
    s
}

/// Render a relative path with `/` separators, regardless of host
/// platform (see `list_dir::path_to_forward_slash`'s rationale).
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

    fn run(dir: &Path, args: &serde_json::Value) -> GrepResult {
        let raw = Grep
            .execute(&args.to_string(), dir)
            .expect("grep must succeed");
        serde_json::from_str(&raw).expect("valid JSON")
    }

    #[test]
    fn declaration_names_the_tool() {
        let decl = Grep.declaration();
        assert_eq!(decl.name, "grep");
        let required = decl.parameters.as_json()["required"]
            .as_array()
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }

    #[test]
    fn finds_matches_with_line_numbers() {
        let dir = tempdir().unwrap();
        seed(
            dir.path(),
            "src/main.rs",
            b"fn other() {}\nfn main() {\n    println!();\n}\n",
        );
        let result = run(dir.path(), &serde_json::json!({ "pattern": "fn main" }));
        assert_eq!(result.matches.len(), 1);
        let m = &result.matches[0];
        assert_eq!(m.path, "src/main.rs");
        assert_eq!(m.line_number, 2);
        assert!(m.line.contains("fn main()"));
    }

    #[test]
    fn fixed_string_disables_regex() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "f.txt", b"a.b.c\nadbec\n");
        // As a regex, `a.b.c` would match `adbec` too. With
        // fixed_string=true, only the literal first line matches.
        let result = run(
            dir.path(),
            &serde_json::json!({ "pattern": "a.b.c", "fixed_string": true }),
        );
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].line_number, 1);
    }

    #[test]
    fn case_insensitive_matches_uppercase() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "f.txt", b"Hello World\n");
        let result = run(
            dir.path(),
            &serde_json::json!({ "pattern": "hello", "case_insensitive": true }),
        );
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn include_glob_filters_files() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/lib.rs", b"target_pattern\n");
        seed(dir.path(), "src/notes.md", b"target_pattern\n");
        let result = run(
            dir.path(),
            &serde_json::json!({
                "pattern": "target_pattern",
                "include_glob": ["*.rs"]
            }),
        );
        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            std::path::Path::new(&result.matches[0].path)
                .extension()
                .and_then(|e| e.to_str()),
            Some("rs")
        );
    }

    #[test]
    fn exclude_glob_skips_files() {
        let dir = tempdir().unwrap();
        seed(dir.path(), "src/lib.rs", b"needle\n");
        seed(dir.path(), "tests/it.rs", b"needle\n");
        let result = run(
            dir.path(),
            &serde_json::json!({
                "pattern": "needle",
                "exclude_glob": ["tests/**"]
            }),
        );
        assert_eq!(result.matches.len(), 1);
        assert!(!result.matches[0].path.starts_with("tests/"));
    }

    #[test]
    fn binary_files_are_skipped() {
        let dir = tempdir().unwrap();
        // Embed a NUL byte before "needle" — binary sniff fires.
        seed(dir.path(), "blob.bin", b"abc\0needle\n");
        seed(dir.path(), "text.txt", b"needle\n");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "needle" }));
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "text.txt");
    }

    #[test]
    fn truncates_at_max_results() {
        let dir = tempdir().unwrap();
        let body = "needle\n".repeat(MAX_MATCHES + 50);
        seed(dir.path(), "big.txt", body.as_bytes());
        let result = run(dir.path(), &serde_json::json!({ "pattern": "needle" }));
        assert!(result.truncated);
        assert_eq!(result.total, MAX_MATCHES);
    }

    #[test]
    fn caller_supplied_max_results_is_honored() {
        let dir = tempdir().unwrap();
        let body = "needle\n".repeat(20);
        seed(dir.path(), "big.txt", body.as_bytes());
        let result = run(
            dir.path(),
            &serde_json::json!({ "pattern": "needle", "max_results": 5 }),
        );
        assert!(result.truncated);
        assert_eq!(result.total, 5);
    }

    #[test]
    fn long_lines_are_truncated_with_marker() {
        let dir = tempdir().unwrap();
        let mut long = "x".repeat(MAX_LINE_LENGTH + 100);
        long.push_str("needle\n");
        seed(dir.path(), "line.txt", long.as_bytes());
        let result = run(dir.path(), &serde_json::json!({ "pattern": "needle" }));
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].line.ends_with("..."));
        assert!(result.matches[0].line.len() <= MAX_LINE_LENGTH + 3);
    }

    #[test]
    fn respects_gitignore_by_default() {
        let dir = tempdir().unwrap();
        // Make this a git repo so .gitignore is honored by the walker.
        seed(dir.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        seed(dir.path(), ".gitignore", b"ignored.txt\n");
        seed(dir.path(), "ignored.txt", b"needle\n");
        seed(dir.path(), "kept.txt", b"needle\n");
        let result = run(dir.path(), &serde_json::json!({ "pattern": "needle" }));
        let paths: Vec<&str> = result.matches.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"kept.txt"));
        assert!(!paths.contains(&"ignored.txt"));
    }

    #[test]
    fn refuses_absolute_path() {
        let dir = tempdir().unwrap();
        let err = Grep
            .execute(
                &serde_json::json!({ "pattern": "x", "path": "/etc" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn refuses_parent_escape() {
        let dir = tempdir().unwrap();
        let err = Grep
            .execute(
                &serde_json::json!({ "pattern": "x", "path": "../outside" }).to_string(),
                dir.path(),
            )
            .expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn invalid_regex_is_loud_invalid_arguments() {
        let dir = tempdir().unwrap();
        let err = Grep
            .execute(
                &serde_json::json!({ "pattern": "fn (" }).to_string(),
                dir.path(),
            )
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn missing_root_is_io_error() {
        let dir = tempdir().unwrap();
        let err = Grep
            .execute(
                &serde_json::json!({ "pattern": "x", "path": "missing" }).to_string(),
                dir.path(),
            )
            .expect_err("must fail");
        assert!(matches!(err, ToolError::Io(_)));
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let dir = tempdir().unwrap();
        let err = Grep
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// Table-driven regression — different inputs map to different
    /// expected match counts and verify the wire shape end-to-end.
    #[test]
    fn table_driven_matches() {
        struct Case {
            name: &'static str,
            files: &'static [(&'static str, &'static [u8])],
            args: serde_json::Value,
            expected_count: usize,
        }
        let cases = [
            Case {
                name: "regex matches multiple lines",
                files: &[("a.txt", b"foo\nbar\nfoobar\n")],
                args: serde_json::json!({ "pattern": "foo" }),
                expected_count: 2,
            },
            Case {
                name: "case-sensitive misses",
                files: &[("a.txt", b"FOO\n")],
                args: serde_json::json!({ "pattern": "foo" }),
                expected_count: 0,
            },
            Case {
                name: "anchor at start",
                files: &[("a.txt", b"hello\nbello hello\n")],
                args: serde_json::json!({ "pattern": "^hello" }),
                expected_count: 1,
            },
        ];

        for case in &cases {
            let dir = tempdir().unwrap();
            for (rel, content) in case.files {
                seed(dir.path(), rel, content);
            }
            let result = run(dir.path(), &case.args);
            assert_eq!(
                result.matches.len(),
                case.expected_count,
                "case '{}' failed: {:?}",
                case.name,
                result.matches
            );
        }
    }
}
