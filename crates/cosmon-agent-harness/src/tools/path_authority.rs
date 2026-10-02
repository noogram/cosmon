// SPDX-License-Identifier: AGPL-3.0-only

//! One path-authority policy shared by every file tool.
//!
//! The file tools used to decide path authority independently:
//! `read_file` checked only the lexical shape of the path, `edit_file` and
//! `write_file` each carried a private copy of a symlink check, and the
//! three walkers (`list_dir`, `grep`, `find_file`) skipped an entry only when
//! its canonical form could be computed and was outside the root — an entry
//! whose canonicalisation failed fell through to the open. This module is
//! the single place that decides, so a tool cannot be more permissive than
//! its siblings.
//!
//! Policy, in every operation: the path must be relative and free of `..`
//! (see [`sanitize_join`]); its canonical form must lie inside the canonical
//! work root; and any failure to establish that canonical form refuses the
//! operation. Nothing falls through to an open.
//!
//! Limits: the check and the later open are two system calls. A concurrent
//! writer that swaps a path component between them can still win that race,
//! and none of this constrains the persistent shell. See
//! `docs/guides/inprocess-harness.md`.

use std::path::{Path, PathBuf};

use crate::tool::{sanitize_join, ToolError};

/// Canonical form of the work root, the reference every other check
/// compares against.
///
/// # Errors
///
/// [`ToolError::Io`] when the root cannot be canonicalised.
pub fn canonical_root(work_dir: &Path) -> Result<PathBuf, ToolError> {
    std::fs::canonicalize(work_dir)
        .map_err(|e| ToolError::Io(format!("canonicalize work_dir: {e}")))
}

/// Resolve `rel` to the canonical path of an **existing** entry inside the
/// work root, for operations that read content or enumerate a directory.
///
/// A symlink whose canonical target stays inside the root is accepted; one
/// that leaves it, or that dangles, is refused, as is any path whose
/// canonical form cannot be computed. The returned path is canonical, so
/// the caller opens exactly what was checked.
///
/// # Errors
///
/// [`ToolError::PathEscape`] for a lexical escape or a canonical target
/// outside the root; [`ToolError::Io`] when the entry does not exist or
/// cannot be canonicalised.
pub fn resolve_existing(work_dir: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let target = sanitize_join(work_dir, rel)?;
    let root = canonical_root(work_dir)?;
    let canonical = std::fs::canonicalize(&target)
        .map_err(|e| ToolError::Io(format!("cannot resolve {rel}: {e}")))?;
    if !canonical.starts_with(&root) {
        return Err(ToolError::PathEscape(format!(
            "path escapes work_dir via symlink: {rel}"
        )));
    }
    Ok(canonical)
}

/// Decide whether a walker entry may be reported or opened.
///
/// Returns `false` when the entry's canonical form is outside
/// `canonical_root` **or cannot be computed**. Walkers skip such entries.
#[must_use]
pub fn entry_allowed(canonical_root: &Path, entry: &Path) -> bool {
    std::fs::canonicalize(entry).is_ok_and(|c| c.starts_with(canonical_root))
}

/// Resolve `rel` to a path a write may target: the entry itself may be
/// absent (create), but must not be a symlink, and its deepest existing
/// ancestor must canonicalise inside the work root.
///
/// Existence is probed with `symlink_metadata`, so a dangling ancestor link
/// is seen and checked rather than skipped as "missing"; any probe error
/// other than not-found refuses.
///
/// # Errors
///
/// [`ToolError::PathEscape`] for a lexical escape, a symlink at the target,
/// or an ancestor resolving outside the root; [`ToolError::Io`] when a
/// probe or canonicalisation fails.
pub fn resolve_write_target(work_dir: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let target = sanitize_join(work_dir, rel)?;
    ensure_write_target_inside(work_dir, &target)?;
    Ok(target)
}

/// Check an already-joined write `target` against the work root. See
/// [`resolve_write_target`].
///
/// # Errors
///
/// As [`resolve_write_target`].
pub fn ensure_write_target_inside(work_dir: &Path, target: &Path) -> Result<(), ToolError> {
    let root = canonical_root(work_dir)?;

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
        match std::fs::symlink_metadata(&probe) {
            Ok(_) => {
                let canonical = std::fs::canonicalize(&probe).map_err(|e| {
                    ToolError::PathEscape(format!("cannot resolve {}: {e}", probe.display()))
                })?;
                if !canonical.starts_with(&root) {
                    return Err(ToolError::PathEscape(format!(
                        "path escapes work_dir via symlink: {}",
                        target.display()
                    )));
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match probe.parent() {
                Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
                _ => {
                    return Err(ToolError::PathEscape(format!(
                        "cannot resolve any ancestor of {}",
                        target.display()
                    )));
                }
            },
            Err(e) => {
                return Err(ToolError::PathEscape(format!(
                    "cannot inspect {}: {e}",
                    probe.display()
                )));
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn resolve_existing_accepts_inside_and_refuses_outside_link() {
        let work = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(work.path().join("a"), "x").unwrap();
        std::fs::write(outside.path().join("s"), "x").unwrap();
        symlink(outside.path().join("s"), work.path().join("out")).unwrap();
        assert!(resolve_existing(work.path(), "a").is_ok());
        assert!(resolve_existing(work.path(), "out").is_err());
        assert!(resolve_existing(work.path(), "missing").is_err());
    }

    #[test]
    fn dangling_ancestor_is_not_treated_as_missing() {
        let work = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path().join("nowhere"), work.path().join("d")).unwrap();
        let err = resolve_write_target(work.path(), "d/new.txt").expect_err("must refuse");
        assert!(matches!(err, ToolError::PathEscape(_)), "{err}");
    }

    #[test]
    fn entry_allowed_is_false_when_canonicalisation_fails() {
        let work = tempdir().unwrap();
        let root = canonical_root(work.path()).unwrap();
        assert!(!entry_allowed(&root, &work.path().join("absent")));
    }
}
