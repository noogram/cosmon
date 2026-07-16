// SPDX-License-Identifier: AGPL-3.0-only

//! Emit side of the neurion auto-registration pipeline.
//!
//! Whenever `cs` runs inside a repo that neurion may not know about, we
//! append a single JSONL line describing the repo to
//! `~/.local/share/neurion/auto-register.jsonl` (platform data dir).
//! `neurion-mcp` drains that file on its next boot and merges the hints
//! into its `repos` table.
//!
//! Failures here are **silent and non-fatal** — cosmon's work is not
//! blocked by the nervous-system registrar.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use neurion_core::auto_register::{
    is_excluded_repo_path, AutoRegisterHint, AUTO_REGISTER_ENV, AUTO_REGISTER_FILENAME,
};

/// Emit an auto-register hint for the current working directory.
///
/// Convenience wrapper used by command handlers. The heavy lifting lives
/// in [`emit_hint_for`], which is easier to test.
pub(crate) fn emit_for_cwd(source: &str) {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let _ = emit_hint_for(&cwd, source);
}

/// Emit an auto-register hint for `start_dir`.
///
/// Walks up from `start_dir` looking for a real `.git` directory. If the
/// path is inside a worktree (`.worktrees/` or `.git/` segments) we return
/// without writing anything — this is the recursion guard described in
/// [`is_excluded_repo_path`].
pub(crate) fn emit_hint_for(start_dir: &Path, source: &str) -> anyhow::Result<bool> {
    let path = hint_file_path()?;
    emit_hint_to(start_dir, source, &path)
}

/// Lower-level emit entry point — writes to `hint_path` instead of resolving
/// it. Exposed for tests that need to avoid the shared process-wide env var
/// `NEURION_AUTO_REGISTER_FILE` (cargo runs tests in parallel; env vars are
/// global state).
pub(crate) fn emit_hint_to(
    start_dir: &Path,
    source: &str,
    hint_path: &Path,
) -> anyhow::Result<bool> {
    if is_excluded_repo_path(start_dir) {
        return Ok(false);
    }
    let Some(root) = find_repo_root(start_dir) else {
        return Ok(false);
    };

    let name = match root.file_name().and_then(|o| o.to_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return Ok(false),
    };
    let local_path = root.to_string_lossy().into_owned();
    let remote_url = git_remote_url(&root);

    let hint = AutoRegisterHint {
        name,
        local_path,
        remote_url,
        source: source.to_string(),
        detected_at: Utc::now().to_rfc3339(),
    };

    append_line(hint_path, &hint)?;
    Ok(true)
}

/// Resolve the hint file path. Honors the [`AUTO_REGISTER_ENV`] override
/// so tests can redirect writes to a sandbox.
pub(crate) fn hint_file_path() -> anyhow::Result<PathBuf> {
    if let Ok(raw) = std::env::var(AUTO_REGISTER_ENV) {
        if !raw.is_empty() {
            return Ok(PathBuf::from(raw));
        }
    }
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join(AUTO_REGISTER_FILENAME))
}

/// Walk up from `start` looking for a directory that contains `.git` as a
/// directory (not a file — that would be a worktree). Returns `None` if
/// no such ancestor exists or if the path is excluded.
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    if is_excluded_repo_path(start) {
        return None;
    }
    let mut cur: Option<&Path> = Some(start);
    while let Some(p) = cur {
        let git = p.join(".git");
        if git.is_dir() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

fn git_remote_url(dir: &Path) -> Option<String> {
    Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            } else {
                None
            }
        })
}

/// Append one JSONL line to `path`, creating parent directories as
/// needed. We don't lock the file: concurrent writers producing
/// interleaved lines would corrupt JSON, but in practice hints are small
/// (<1 KiB) and POSIX `O_APPEND` writes of that size are atomic.
fn append_line(path: &Path, hint: &AutoRegisterHint) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(hint)?;
    line.push('\n');
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(dir: &Path) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .expect("git init");
        assert!(status.success());
    }

    #[test]
    fn emit_writes_jsonl_for_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let hint_file = tmp.path().join("hint.jsonl");
        let wrote = emit_hint_to(&repo, "test", &hint_file).unwrap();

        assert!(wrote);
        assert!(hint_file.exists(), "hint file should be created");
        let content = fs::read_to_string(&hint_file).unwrap();
        let line = content.lines().next().expect("at least one line");
        let hint: AutoRegisterHint = serde_json::from_str(line).unwrap();
        assert_eq!(hint.name, "myrepo");
        assert_eq!(hint.source, "test");
        // remote_url is None because no origin configured
        assert!(hint.remote_url.is_none());
    }

    #[test]
    fn emit_skips_worktree_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        let wt = repo.join(".worktrees").join("task-1");
        fs::create_dir_all(&wt).unwrap();

        let hint_file = tmp.path().join("hint.jsonl");
        let wrote = emit_hint_to(&wt, "test", &hint_file).unwrap();

        assert!(!wrote);
        assert!(!hint_file.exists());
    }

    #[test]
    fn emit_skips_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("not-a-repo");
        fs::create_dir_all(&plain).unwrap();

        let hint_file = tmp.path().join("hint.jsonl");
        let wrote = emit_hint_to(&plain, "test", &hint_file).unwrap();

        assert!(!wrote);
        assert!(!hint_file.exists());
    }

    #[test]
    fn emit_appends_to_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let hint_file = tmp.path().join("hint.jsonl");
        emit_hint_to(&repo, "first", &hint_file).unwrap();
        emit_hint_to(&repo, "second", &hint_file).unwrap();

        let content = fs::read_to_string(&hint_file).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }
}
