// SPDX-License-Identifier: AGPL-3.0-only

//! `edit_file` — Aider-style exact-match search-and-replace tool.
//!
//! The design combines an `EditOp` struct, the unique-match-or-fail
//! argument, and the Aider precedent of "two days of operator pain
//! training the prompt, then the model gets it right."
//!
//! # The discipline
//!
//! The model emits one or more [`EditOp`]s, each `{ path, search,
//! replace }`. The tool refuses unless `search` matches **byte-for-byte
//! exactly once** in `path`:
//!
//! - **Zero matches** → [`EditError::NoMatch`]; the model re-reads
//!   the file and corrects its query.
//! - **Two or more matches** → [`EditError::Ambiguous`]; the model
//!   adds surrounding context until the match is unique. This is the
//!   load-bearing rule that prevents silent wrong-line edits.
//! - **Empty `search`** → "create file" semantics. The target must
//!   not exist; otherwise [`EditError::AlreadyExists`].
//! - **Empty `replace`** → "delete that block" semantics; the matched
//!   range is removed.
//!
//! No fuzzy whitespace, no leading/trailing tolerance, no newline
//! normalisation. UTF-8 only. Path safety is enforced by
//! [`crate::tool::sanitize_join`] — absolute paths and `..` segments
//! are loud failures via [`crate::tool::ToolError::PathEscape`], not
//! per-op errors the model could shrug off.
//!
//! # Call-level transactional
//!
//! [`EditParams::edits`] are grouped by `path`, then for each file
//! applied sequentially in-memory before any commit hits disk. Two
//! layers of all-or-nothing:
//!
//! - **Per-file** — if any op for a file fails (`NoMatch` /
//!   `Ambiguous` / `AlreadyExists`), the whole file's changes are
//!   aborted; no partial state on disk for that file.
//! - **Call-level** — if any
//!   file in the batch fails, NO file is committed. A retrying model
//!   never double-applies an op-A on file-A just because op-B on
//!   file-B failed. Successful per-file `EditResult`s in the returned
//!   vector indicate "would have been applied if the batch had
//!   succeeded"; on failure the disk stays byte-identical.
//!
//! The model receives one [`Result<EditResult, EditError>`] per
//! unique `path` (first-appearance order), serialised as JSON in the
//! tool result string.
//!
//! # Symlink safety
//!
//! After [`crate::tool::sanitize_join`] rejects absolute paths and
//! `..` segments, each resolved `target` is canonicalised (along
//! with its deepest existing ancestor) and asserted to live inside
//! the canonicalised `work_dir`. A symlink at `work_dir/out`
//! pointing at `~/.ssh/authorized_keys` is refused via
//! [`ToolError::PathEscape`], matching the loud-failure discipline
//! the rest of the v0 tool surface enforces.
//!
//! # Atomic rename
//!
//! The legacy `write_atomic` was `std::fs::write`, which is **not**
//! atomic — a SIGKILL mid-write left a truncated file on disk. The
//! replacement writes to `target.<nonce>.tmp`, fsyncs implicitly via the
//! close, then `std::fs::rename(tmp, target)`. POSIX rename is
//! atomic on the same filesystem; the target either has the old
//! content or the new content, never a truncated mix.
//!
//! # Out of scope (v0)
//!
//! - Unified diff support — promotion to v1 if exact-match misses
//!   cluster on files >800 lines (synthesis D2 follow-up).
//! - `base_blake3` seal (architect's SF-6) — separate bead.
//! - Three-way merge / `.rej` files / AST awareness / edit history —
//!   git is the undo (torvalds §Q1 explicit).
//! - Fuzzy matching, whitespace tolerance, encoding conversion —
//!   strictness IS the discipline.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tool::{sanitize_join, ParametersSchema, Tool, ToolDeclaration, ToolError};

/// One search-and-replace op against one file.
///
/// All three fields are required, including `replace` (empty string
/// = "delete the matched block"). `search == ""` means "create the
/// file with `replace` as its contents."
///
/// `#[non_exhaustive]` — additive
/// fields (e.g. `base_blake3` seal — SF-6 from ADR-102) must not
/// require a major bump.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditOp {
    /// Path relative to `work_dir`; sanitized by
    /// [`crate::tool::sanitize_join`].
    pub path: String,
    /// Byte-for-byte exact text to find. Empty string switches to
    /// create-file semantics.
    pub search: String,
    /// Replacement text. Empty string deletes the matched block.
    pub replace: String,
}

/// JSON envelope the model emits to invoke `edit_file`.
///
/// `#[non_exhaustive]` — keeps
/// future fields (e.g. transaction options) non-breaking.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditParams {
    /// Ordered list of ops. Grouped by `path` server-side, then
    /// applied transactionally per file.
    pub edits: Vec<EditOp>,
}

/// Per-file success summary returned to the model.
///
/// `#[non_exhaustive]` — additive
/// fields (e.g. timing, byte-level diffs) must not require a major
/// bump.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    /// The file's `path` (as supplied in the [`EditOp`]).
    pub path: String,
    /// Signed delta of `final_len - original_len` in bytes. Negative
    /// for net-deletion, positive for net-addition.
    pub delta_bytes: i64,
    /// Human-readable summary of the ops applied to this file.
    pub summary: String,
}

/// Per-file failure surface. Different from [`ToolError`] because
/// these are *recoverable by the model* (it can retry with corrected
/// search context). Structural failures (path escape, malformed JSON)
/// still go through [`ToolError`] to enforce loud refusal.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// `search` was not found in `path`. The model should re-read
    /// the file and emit a corrected query.
    #[error("search string not found in {path}")]
    NoMatch {
        /// File path the failed op targeted.
        path: String,
    },
    /// `search` matched `count` (>= 2) distinct ranges in `path`.
    /// The model must include more surrounding context until the
    /// match is unique. This is the load-bearing discipline.
    #[error("search string matched {count} times in {path}; include more context")]
    Ambiguous {
        /// File path the failed op targeted.
        path: String,
        /// Number of matches found (always >= 2).
        count: usize,
    },
    /// `search == ""` was supplied to create a new file, but the
    /// target already exists. The model should issue a search/replace
    /// op instead.
    #[error("file {path} already exists (use search/replace, not create)")]
    AlreadyExists {
        /// File path the failed op targeted.
        path: String,
    },
    /// Filesystem IO failed while reading or writing the file. The
    /// inner string is the underlying `std::io::Error` description.
    #[error("io: {0}")]
    Io(String),
}

/// The whitelisted `edit_file` capability — see the module docs for
/// the discipline.
#[derive(Debug, Default, Clone, Copy)]
pub struct EditFile;

impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "edit_file",
            description: "Exact-match search-and-replace inside files in the worker's work_dir. \
                Each EditOp requires `search` to match byte-for-byte exactly ONCE in `path`; \
                two-or-more matches return Ambiguous and force the caller to include more \
                surrounding context. Empty `search` creates a new file (which must not exist). \
                Empty `replace` deletes the matched block. Multiple ops to the same file are \
                applied transactionally — all succeed or none are written.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "description": "Ordered list of edit ops. Ops to the same file are \
                            grouped and applied transactionally per file.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Path relative to work_dir."
                                },
                                "search": {
                                    "type": "string",
                                    "description": "Byte-for-byte exact text to find. \
                                        Empty string = create file."
                                },
                                "replace": {
                                    "type": "string",
                                    "description": "Replacement text. \
                                        Empty string = delete the matched block."
                                }
                            },
                            "required": ["path", "search", "replace"]
                        }
                    }
                },
                "required": ["edits"]
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: EditParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "edit_file".to_owned(),
                message: e.to_string(),
            })?;
        let results = apply_edits(work_dir, &params)?;
        serde_json::to_string(&results).map_err(|e| ToolError::Io(e.to_string()))
    }
}

/// Apply an [`EditParams`] batch against `work_dir`.
///
/// Returns one [`Result<EditResult, EditError>`] per unique `path`
/// in first-appearance order. Structural failures (path escape,
/// invalid argument) still bubble out as [`ToolError`].
///
/// Extracted from [`EditFile::execute`] so tests can inspect typed
/// [`EditError`] variants without round-tripping through JSON.
///
/// # Errors
///
/// Returns [`ToolError::PathEscape`] if any `EditOp.path` is absolute
/// or escapes `work_dir`.
pub fn apply_edits(
    work_dir: &Path,
    params: &EditParams,
) -> Result<Vec<Result<EditResult, EditError>>, ToolError> {
    // First pass: validate every path BEFORE touching the filesystem.
    // A single path escape fails the whole call loudly (forgemaster
    // §3.3) — never converted into a per-op error the model could
    // ignore.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<EditOp>> = BTreeMap::new();
    for op in &params.edits {
        let _ = sanitize_join(work_dir, &op.path)?;
        if !groups.contains_key(&op.path) {
            order.push(op.path.clone());
        }
        groups.entry(op.path.clone()).or_default().push(op.clone());
    }

    // Second pass: compute per-file outcomes in RAM. Successful files
    // produce a `(target, new_bytes)` commit deferred until every
    // file in the batch has succeeded — that's the call-level
    // transactionality W5 ships (delib-20260519-e6db / knuth §K2).
    let mut results: Vec<Result<EditResult, EditError>> = Vec::with_capacity(order.len());
    let mut commits: Vec<(PathBuf, Vec<u8>)> = Vec::with_capacity(order.len());
    let mut any_error = false;
    for path in &order {
        let ops = groups.remove(path).unwrap_or_default();
        let target = sanitize_join(work_dir, path)?;
        // Symlink-safety check (W5 / adversary §F5.2) — must run
        // before the read, so a symlink target outside `work_dir`
        // never sees a `read_to_string` call.
        ensure_inside_work_dir(work_dir, &target)?;
        match compute_file_ops(path, &target, &ops) {
            Ok((result, new_bytes)) => {
                commits.push((target, new_bytes));
                results.push(Ok(result));
            }
            Err(e) => {
                any_error = true;
                results.push(Err(e));
            }
        }
    }

    // Third pass: commit only if every file succeeded (W5 call-level
    // transactionality). On any error the disk stays byte-identical
    // — even files whose own ops were valid stay untouched. The
    // model sees per-file `Result`s and retries the whole batch.
    if !any_error {
        for (target, bytes) in commits {
            write_via_rename(&target, &bytes)
                .map_err(|e| ToolError::Io(format!("{}: {}", target.display(), e)))?;
        }
    }
    Ok(results)
}

/// Compute the final byte payload for one file's edit ops.
///
/// Pure / no-IO except the initial `read_to_string` — the commit hits
/// disk only after every file in the batch has succeeded. Returns
/// `(EditResult, new_bytes)` on success; the caller is responsible
/// for either writing `new_bytes` to `target` or discarding it
/// (call-level transactionality).
fn compute_file_ops(
    rel_path: &str,
    target: &Path,
    ops: &[EditOp],
) -> Result<(EditResult, Vec<u8>), EditError> {
    let original = match std::fs::read_to_string(target) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(EditError::Io(e.to_string())),
    };
    let original_len = original.as_ref().map_or(0_usize, std::string::String::len);
    let mut content = original;
    let mut summary_lines: Vec<String> = Vec::with_capacity(ops.len());

    for op in ops {
        if op.search.is_empty() {
            if content.is_some() {
                return Err(EditError::AlreadyExists {
                    path: rel_path.to_owned(),
                });
            }
            summary_lines.push(format!("created with {} bytes", op.replace.len()));
            content = Some(op.replace.clone());
            continue;
        }

        let current = content.as_ref().ok_or_else(|| EditError::NoMatch {
            path: rel_path.to_owned(),
        })?;
        let count = count_occurrences(current, op.search.as_str());
        match count {
            0 => {
                return Err(EditError::NoMatch {
                    path: rel_path.to_owned(),
                });
            }
            1 => {
                let new_content = current.replacen(op.search.as_str(), op.replace.as_str(), 1);
                summary_lines.push(format!(
                    "replaced 1 occurrence ({} -> {} bytes)",
                    op.search.len(),
                    op.replace.len()
                ));
                content = Some(new_content);
            }
            n => {
                return Err(EditError::Ambiguous {
                    path: rel_path.to_owned(),
                    count: n,
                });
            }
        }
    }

    let final_content = content.unwrap_or_default();
    // Reachable saturation: i64 cannot hold lengths > i64::MAX. For
    // practical edit_file payloads (kilobytes to megabytes) the cast
    // is exact; saturation is a guard against pathological inputs.
    let delta_bytes = i64::try_from(final_content.len()).unwrap_or(i64::MAX)
        - i64::try_from(original_len).unwrap_or(i64::MAX);

    let result = EditResult {
        path: rel_path.to_owned(),
        delta_bytes,
        summary: summary_lines.join("; "),
    };
    Ok((result, final_content.into_bytes()))
}

/// Refuse a `target` whose canonical path (or any ancestor's
/// canonical path) escapes `work_dir` via a symlink or an absolute
/// link. Complements [`sanitize_join`], which only catches
/// lexical `..` segments and absolute components — a previous turn's
/// `exec_command "ln -s ~/.ssh/authorized_keys ./out"` would otherwise
/// let the current turn's `edit_file path=out` overwrite the
/// operator's SSH key via `std::fs::write`'s follow-symlink default.
fn ensure_inside_work_dir(work_dir: &Path, target: &Path) -> Result<(), ToolError> {
    let canonical_work_dir = std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))?;

    // Reject the target outright if it exists as a symlink (even a
    // dangling one). `std::fs::symlink_metadata` does NOT follow
    // links — `Path::is_symlink` is the equivalent shortcut on
    // current stable.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            return Err(ToolError::PathEscape(format!(
                "symlink target refused: {}",
                target.display()
            )));
        }
    }

    // Walk up to the deepest existing ancestor and verify its
    // canonical form (with all symlinks resolved) stays inside the
    // canonical `work_dir`. This catches ancestor symlinks pointing
    // outside the worktree without requiring the target itself to
    // exist (create-file ops legitimately reach here).
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

/// Count occurrences of `needle` in `haystack`, including positions that
/// overlap one another. `str::matches(...).count()` walks the iterator
/// in *non-overlapping* steps — for `"aa"` in `"aaaaa"` it reports 2,
/// not the 4 byte-positions where the pattern actually starts. This
/// undercount feeds the wrong number into [`EditError::Ambiguous`] and,
/// worse, lets a 3-occurrence input slip past the `>= 2` ambiguity gate
/// when only 1 non-overlapping run is found (knuth K4).
///
/// Returns 0 if `needle` is empty so callers do not have to special-case
/// it; ambiguity for an empty search would be infinite by definition.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut count = 0_usize;
    let mut i = 0_usize;
    while i + n.len() <= h.len() {
        if &h[i..i + n.len()] == n {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Write `bytes` to `target` atomically by writing a sibling tmp file
/// first, then renaming on top. POSIX rename is atomic on the same
/// filesystem — the target either has the old content or the new
/// content, never a truncated mix. Replaces the v0 `std::fs::write`-based
/// `write_atomic`, which was a name-only contract.
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
    // Write the full body to tmp; close it so the file is durable on
    // most filesystems before we rename. `fs::write` already drops
    // the File handle at end-of-call.
    match std::fs::write(&tmp_path, bytes) {
        Ok(()) => {}
        Err(e) => {
            // Best-effort cleanup — leaving a stray tmp file behind
            // would leak across calls otherwise.
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

/// Convenience: resolve the on-disk path the way [`EditFile`] does,
/// for tests and debug surfaces.
#[must_use]
pub fn resolve(work_dir: &Path, rel: &str) -> Option<PathBuf> {
    sanitize_join(work_dir, rel).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_seed(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("seed parent");
        }
        std::fs::write(&p, content).expect("seed write");
        p
    }

    #[test]
    fn unique_match_replace_succeeds() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "src/main.rs", "let x = 1;\nlet y = 2;\n");
        let params = EditParams {
            edits: vec![EditOp {
                path: "src/main.rs".to_owned(),
                search: "let x = 1;".to_owned(),
                replace: "let x = 42;".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        assert_eq!(results.len(), 1);
        let ok = results[0].as_ref().expect("ok variant");
        assert_eq!(ok.path, "src/main.rs");
        assert_eq!(ok.delta_bytes, 1); // "42" - "1" => +1 byte
        let final_content =
            std::fs::read_to_string(dir.path().join("src/main.rs")).expect("read back");
        assert_eq!(final_content, "let x = 42;\nlet y = 2;\n");
    }

    #[test]
    fn no_match_surfaces_no_match_error() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "foo.txt", "alpha beta gamma");
        let params = EditParams {
            edits: vec![EditOp {
                path: "foo.txt".to_owned(),
                search: "DELTA".to_owned(),
                replace: "x".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let err = results[0].as_ref().expect_err("must fail");
        assert!(matches!(err, EditError::NoMatch { path } if path == "foo.txt"));
        // Original file must be untouched.
        let after = std::fs::read_to_string(dir.path().join("foo.txt")).expect("untouched");
        assert_eq!(after, "alpha beta gamma");
    }

    #[test]
    fn ambiguous_match_reports_count() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "dup.txt", "abc\nabc\nabc\n");
        let params = EditParams {
            edits: vec![EditOp {
                path: "dup.txt".to_owned(),
                search: "abc".to_owned(),
                replace: "xyz".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let err = results[0].as_ref().expect_err("must fail");
        match err {
            EditError::Ambiguous { path, count } => {
                assert_eq!(path, "dup.txt");
                assert_eq!(*count, 3);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_two_matches_reports_count_two() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "two.txt", "foo bar foo");
        let params = EditParams {
            edits: vec![EditOp {
                path: "two.txt".to_owned(),
                search: "foo".to_owned(),
                replace: "baz".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let err = results[0].as_ref().expect_err("must fail");
        let count = match err {
            EditError::Ambiguous { count, .. } => *count,
            other => panic!("expected Ambiguous, got {other:?}"),
        };
        assert_eq!(count, 2);
    }

    #[test]
    fn empty_search_creates_new_file() {
        let dir = tempdir().unwrap();
        let params = EditParams {
            edits: vec![EditOp {
                path: "new/nested.txt".to_owned(),
                search: String::new(),
                replace: "hello world".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let ok = results[0].as_ref().expect("ok variant");
        assert_eq!(ok.delta_bytes, 11);
        let written =
            std::fs::read_to_string(dir.path().join("new/nested.txt")).expect("must exist");
        assert_eq!(written, "hello world");
    }

    #[test]
    fn empty_search_on_existing_file_already_exists() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "there.txt", "pre-existing");
        let params = EditParams {
            edits: vec![EditOp {
                path: "there.txt".to_owned(),
                search: String::new(),
                replace: "new contents".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let err = results[0].as_ref().expect_err("must fail");
        assert!(matches!(err, EditError::AlreadyExists { path } if path == "there.txt"));
        let after = std::fs::read_to_string(dir.path().join("there.txt")).expect("untouched");
        assert_eq!(after, "pre-existing");
    }

    #[test]
    fn empty_replace_deletes_block() {
        let dir = tempdir().unwrap();
        write_seed(
            dir.path(),
            "code.rs",
            "fn main() {\n    println!(\"hello\");\n    // TODO: remove\n}\n",
        );
        let params = EditParams {
            edits: vec![EditOp {
                path: "code.rs".to_owned(),
                search: "    // TODO: remove\n".to_owned(),
                replace: String::new(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let ok = results[0].as_ref().expect("ok variant");
        assert_eq!(ok.delta_bytes, -20); // length of the removed line
        let after = std::fs::read_to_string(dir.path().join("code.rs")).expect("read");
        assert_eq!(after, "fn main() {\n    println!(\"hello\");\n}\n");
    }

    #[test]
    fn path_escape_parent_dir_refused_loudly() {
        let dir = tempdir().unwrap();
        let params = EditParams {
            edits: vec![EditOp {
                path: "../escape.txt".to_owned(),
                search: String::new(),
                replace: "x".to_owned(),
            }],
        };
        let err = apply_edits(dir.path(), &params).expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)), "got {err:?}");
    }

    #[test]
    fn path_escape_absolute_refused_loudly() {
        let dir = tempdir().unwrap();
        let params = EditParams {
            edits: vec![EditOp {
                path: "/etc/passwd".to_owned(),
                search: String::new(),
                replace: "x".to_owned(),
            }],
        };
        let err = apply_edits(dir.path(), &params).expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)), "got {err:?}");
    }

    #[test]
    fn multi_op_transactional_on_failure_no_disk_write() {
        let dir = tempdir().unwrap();
        let seed = "AAA\nBBB\nCCC\n";
        write_seed(dir.path(), "tx.txt", seed);
        // Three ops on one file; op #2 will Ambiguous (BBB does not
        // appear twice — we craft a search that matches three times)
        // — actually pick a search that exists 0 times to trigger
        // NoMatch on op #2. Op #1 and op #3 are valid unique matches.
        let params = EditParams {
            edits: vec![
                EditOp {
                    path: "tx.txt".to_owned(),
                    search: "AAA".to_owned(),
                    replace: "aaa".to_owned(),
                },
                EditOp {
                    path: "tx.txt".to_owned(),
                    search: "ZZZ_missing".to_owned(),
                    replace: "x".to_owned(),
                },
                EditOp {
                    path: "tx.txt".to_owned(),
                    search: "CCC".to_owned(),
                    replace: "ccc".to_owned(),
                },
            ],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        assert_eq!(results.len(), 1);
        let err = results[0].as_ref().expect_err("file must fail as a whole");
        assert!(matches!(err, EditError::NoMatch { .. }));
        // Disk content unchanged — neither op #1 nor op #3 were
        // persisted.
        let after = std::fs::read_to_string(dir.path().join("tx.txt")).expect("read");
        assert_eq!(after, seed, "file must be byte-identical to seed");
    }

    #[test]
    fn multi_op_same_file_succeeds_all_or_nothing() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "ok.txt", "AAA\nBBB\nCCC\n");
        let params = EditParams {
            edits: vec![
                EditOp {
                    path: "ok.txt".to_owned(),
                    search: "AAA".to_owned(),
                    replace: "a1".to_owned(),
                },
                EditOp {
                    path: "ok.txt".to_owned(),
                    search: "CCC".to_owned(),
                    replace: "c1".to_owned(),
                },
            ],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let ok = results[0].as_ref().expect("ok variant");
        assert_eq!(ok.path, "ok.txt");
        let after = std::fs::read_to_string(dir.path().join("ok.txt")).expect("read");
        assert_eq!(after, "a1\nBBB\nc1\n");
    }

    /// Call-level transactionality. When op-A on `a.txt` succeeds but
    /// op-B on `b.txt` fails, the disk for `a.txt` must stay
    /// byte-identical to the seed. The earlier contract was per-file
    /// independent and silently double-applied `a.txt` on retry.
    #[test]
    fn batch_failure_leaves_all_files_byte_identical() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "a.txt", "hello");
        write_seed(dir.path(), "b.txt", "world");
        let params = EditParams {
            edits: vec![
                EditOp {
                    path: "a.txt".to_owned(),
                    search: "hello".to_owned(),
                    replace: "HI".to_owned(),
                },
                EditOp {
                    path: "b.txt".to_owned(),
                    search: "MISSING".to_owned(),
                    replace: "x".to_owned(),
                },
            ],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        assert_eq!(results.len(), 2);
        // The per-file result vector still surfaces each file's
        // outcome — Ok for a.txt (would have applied), Err for b.txt.
        assert!(results.iter().any(std::result::Result::is_ok));
        assert!(results.iter().any(std::result::Result::is_err));
        // Both files unchanged on disk — call-level transactionality.
        let a_after = std::fs::read_to_string(dir.path().join("a.txt")).expect("read");
        assert_eq!(
            a_after, "hello",
            "a.txt must stay byte-identical on batch failure"
        );
        let b_after = std::fs::read_to_string(dir.path().join("b.txt")).expect("read");
        assert_eq!(b_after, "world");
    }

    /// A symlink at
    /// `work_dir/out` pointing outside the worktree must be refused
    /// with `ToolError::PathEscape`. The legacy
    /// `sanitize_join` only caught lexical `..` and absolute paths;
    /// `std::fs::write` would happily follow a symlink and clobber
    /// `~/.ssh/authorized_keys`.
    #[test]
    fn symlink_target_outside_work_dir_refused() {
        let work = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "do not overwrite").expect("seed");

        // Create a symlink work_dir/out -> outside/secret.txt.
        // Unix-only; the harness is Unix-only by design (see
        // exec_command.rs module docs).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, work.path().join("out")).expect("symlink");

        #[cfg(unix)]
        {
            let params = EditParams {
                edits: vec![EditOp {
                    path: "out".to_owned(),
                    search: "do not overwrite".to_owned(),
                    replace: "owned".to_owned(),
                }],
            };
            let err = apply_edits(work.path(), &params).expect_err("must refuse symlink");
            assert!(matches!(err, ToolError::PathEscape(_)), "got {err:?}");
            // The actual file outside the worktree is untouched.
            let after = std::fs::read_to_string(&secret).expect("read");
            assert_eq!(after, "do not overwrite");
        }
    }

    /// An ancestor
    /// symlink (subdir → outside) must also be refused.
    #[test]
    fn ancestor_symlink_outside_work_dir_refused() {
        let work = tempdir().unwrap();
        let outside = tempdir().unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), work.path().join("escape")).expect("symlink");

        #[cfg(unix)]
        {
            let params = EditParams {
                edits: vec![EditOp {
                    path: "escape/secret.txt".to_owned(),
                    search: String::new(),
                    replace: "owned".to_owned(),
                }],
            };
            let err = apply_edits(work.path(), &params).expect_err("must refuse ancestor symlink");
            assert!(matches!(err, ToolError::PathEscape(_)), "got {err:?}");
        }
    }

    /// Write
    /// goes through a sibling tmp file + `fs::rename`, not a direct
    /// `fs::write`. We assert the tmp pattern indirectly by writing
    /// to a file and confirming the directory contains no leftover
    /// `.tmp` siblings after a successful write.
    #[test]
    fn write_via_rename_leaves_no_tmp_leftovers() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "atomic.txt", "before");
        let params = EditParams {
            edits: vec![EditOp {
                path: "atomic.txt".to_owned(),
                search: "before".to_owned(),
                replace: "after".to_owned(),
            }],
        };
        let _results = apply_edits(dir.path(), &params).expect("apply ok");
        let after = std::fs::read_to_string(dir.path().join("atomic.txt")).expect("read");
        assert_eq!(after, "after");
        // No `.tmp` sibling left behind.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let leftovers: Vec<_> = entries.iter().filter(|n| n.contains(".tmp")).collect();
        assert!(
            leftovers.is_empty(),
            "rename-based atomic write must leave no .tmp siblings, got {entries:?}"
        );
    }

    /// Verify the
    /// rename-helper is reachable as a private function so its
    /// contract (atomic on same FS) is testable directly.
    #[test]
    fn write_via_rename_replaces_target_atomically() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, "before").unwrap();
        write_via_rename(&target, b"after").expect("rename ok");
        let body = std::fs::read_to_string(&target).expect("read");
        assert_eq!(body, "after");
    }

    #[test]
    fn execute_returns_json_serialisable_results() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "j.txt", "alpha");
        let tool = EditFile;
        let args = serde_json::json!({
            "edits": [{
                "path": "j.txt",
                "search": "alpha",
                "replace": "omega"
            }]
        });
        let raw = tool
            .execute(&args.to_string(), dir.path())
            .expect("execute ok");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert!(parsed.is_array());
        // Externally-tagged Result serialises as {"Ok": {...}}.
        let entry = &parsed[0];
        assert!(entry.get("Ok").is_some(), "expected Ok variant: {parsed}");
    }

    #[test]
    fn invalid_arguments_surface_as_tool_error() {
        let dir = tempdir().unwrap();
        let tool = EditFile;
        let err = tool
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// knuth K4 regression — `str::matches(...).count()` walks the
    /// haystack in *non-overlapping* steps. For `"aa"` in `"aaaaa"` it
    /// reports 2; the four byte-positions where the pattern actually
    /// starts (0, 1, 2, 3) is what the ambiguity gate must report so
    /// the model is told the truth about how branchy the match is.
    #[test]
    fn self_overlapping_needle_counts_every_position() {
        assert_eq!(count_occurrences("aaaaa", "aa"), 4);
        assert_eq!(count_occurrences("aaaa", "aa"), 3);
        // Non-overlapping case is unchanged.
        assert_eq!(count_occurrences("abcabc", "abc"), 2);
        // Edge cases.
        assert_eq!(count_occurrences("", "aa"), 0);
        assert_eq!(count_occurrences("aa", ""), 0);
        assert_eq!(count_occurrences("a", "aa"), 0);
    }

    /// End-to-end witness that the ambiguity report carries the
    /// overlap-corrected count. Before this fix, the same input
    /// reported `count: 2` and the prompt-engineered repair operated
    /// on bad data.
    #[test]
    fn ambiguous_self_overlapping_reports_byte_position_count() {
        let dir = tempdir().unwrap();
        write_seed(dir.path(), "ovr.txt", "aaaaa");
        let params = EditParams {
            edits: vec![EditOp {
                path: "ovr.txt".to_owned(),
                search: "aa".to_owned(),
                replace: "bb".to_owned(),
            }],
        };
        let results = apply_edits(dir.path(), &params).expect("apply ok");
        let err = results[0].as_ref().expect_err("must fail Ambiguous");
        match err {
            EditError::Ambiguous { count, .. } => assert_eq!(*count, 4),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn declaration_advertises_required_fields() {
        let decl = EditFile.declaration();
        assert_eq!(decl.name, "edit_file");
        let required = decl.parameters.as_json()["properties"]["edits"]["items"]["required"]
            .as_array()
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["path", "search", "replace"]);
    }
}
