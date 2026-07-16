// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor worktrees` — audit `.worktrees/` for permission and escape hazards.
//!
//! Each cosmon worker gets a dedicated git worktree under
//! `<project_root>/.worktrees/<molecule_id>/`. Those trees are the
//! filesystem that a live agent actually writes to — if they carry
//! world-writable bits, outbound symlinks that escape the project, or
//! untracked files that no formula produced, the sandbox assumption is
//! already broken.
//!
//! Checks performed per worktree:
//!
//! - Mode 0777 on the worktree root or any directory inside it.
//! - Symlinks whose target resolves outside the project root.
//! - Files present in the working tree but not in git index, ignored,
//!   or untracked (potential exfiltration staging / accidental add).
//!
//! All findings are `Severity::Warning` — none of these are
//! "house-on-fire" individually, but together they describe a missing
//! sandbox.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::findings::{Finding, ProbeReport, Severity};

const PROBE: &str = "worktrees";

/// Arguments for `cs doctor worktrees`.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Override the project root (default: git top-level).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Run the worktree audit. `root` is the project root.
///
/// # Errors
/// Returns an error if `root/.worktrees/` cannot be read. Per-worktree
/// errors become `Warning` findings rather than aborting the probe.
#[allow(clippy::unnecessary_wraps)]
pub fn scan(root: &Path) -> anyhow::Result<ProbeReport> {
    let mut report = ProbeReport::new(PROBE);
    let worktrees_root = root.join(".worktrees");
    if !worktrees_root.exists() {
        report.findings.push(Finding::new(
            PROBE,
            Severity::Info,
            "no .worktrees/ directory — nothing to audit",
        ));
        return Ok(report);
    }

    let entries = match fs::read_dir(&worktrees_root) {
        Ok(it) => it,
        Err(e) => {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Warning,
                    format!("cannot read .worktrees/: {e}"),
                )
                .with_path(&worktrees_root),
            );
            return Ok(report);
        }
    };

    for entry in entries.flatten() {
        let wt = entry.path();
        if !wt.is_dir() {
            continue;
        }
        report.scanned += 1;
        audit_worktree(root, &wt, &mut report);
    }
    Ok(report)
}

/// CLI entry point for `cs doctor worktrees`.
///
/// # Errors
/// Returns an error when the project root cannot be resolved (non-git
/// directory without `--root` override).
pub fn run(ctx: &super::Context, args: &Args) -> anyhow::Result<()> {
    let root = match &args.root {
        Some(p) => p.canonicalize().unwrap_or_else(|_| p.clone()),
        None => super::leaks::git_root(&std::env::current_dir()?)?,
    };
    let report = scan(&root)?;
    super::emit_report_and_exit(ctx, &[report])
}

fn audit_worktree(project_root: &Path, wt: &Path, report: &mut ProbeReport) {
    check_world_writable(wt, report);
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    check_symlinks_outward(wt, &canonical_root, report);
    check_untracked_files(wt, report);
}

#[cfg(unix)]
fn check_world_writable(wt: &Path, report: &mut ProbeReport) {
    use std::os::unix::fs::PermissionsExt;
    walk_dirs(wt, &mut |dir: &Path| {
        let Ok(meta) = fs::symlink_metadata(dir) else {
            return;
        };
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o002 != 0 {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Warning,
                    format!("world-writable directory (mode 0{mode:03o})"),
                )
                .with_path(dir)
                .with_remediation(format!(
                    "chmod o-w {} (or 0700 for strict isolation)",
                    dir.display()
                )),
            );
        }
    });
}

#[cfg(not(unix))]
fn check_world_writable(_wt: &Path, _report: &mut ProbeReport) {
    // File mode bits are Unix-only; skip gracefully on other platforms.
}

fn check_symlinks_outward(wt: &Path, project_root: &Path, report: &mut ProbeReport) {
    walk_all(wt, &mut |entry: &Path| {
        let Ok(meta) = fs::symlink_metadata(entry) else {
            return;
        };
        if !meta.file_type().is_symlink() {
            return;
        }
        let Ok(target) = fs::read_link(entry) else {
            return;
        };
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            entry
                .parent()
                .map_or_else(|| target.clone(), |p| p.join(&target))
        };
        let canonical = resolved.canonicalize().unwrap_or(resolved);
        if !canonical.starts_with(project_root) {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Warning,
                    "symlink escapes project root".to_owned(),
                )
                .with_path(entry)
                .with_detail(format!("target: {}", target.display()))
                .with_remediation("Replace with an in-tree copy or a narrower symlink.".to_owned()),
            );
        }
    });
}

fn check_untracked_files(wt: &Path, report: &mut ProbeReport) {
    let out = Command::new("git")
        .args([
            "-C",
            &wt.display().to_string(),
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Info,
                    format!(
                        "git ls-files refused in {}: {}",
                        wt.display(),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                )
                .with_path(wt),
            );
            return;
        }
        Err(e) => {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Info,
                    format!("could not invoke git in {}: {e}", wt.display()),
                )
                .with_path(wt),
            );
            return;
        }
    };
    let count = out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .count();
    if count > 0 {
        report.findings.push(
            Finding::new(
                PROBE,
                Severity::Warning,
                format!(
                    "{count} untracked file{} in worktree (not in index, not ignored)",
                    if count == 1 { "" } else { "s" }
                ),
            )
            .with_path(wt)
            .with_remediation(
                "Review with `git -C <worktree> status`; add to .gitignore or delete before done."
                    .to_owned(),
            ),
        );
    }
}

fn walk_dirs(root: &Path, visit: &mut dyn FnMut(&Path)) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        visit(&dir);
        let Ok(iter) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in iter.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                stack.push(path);
            }
        }
    }
}

fn walk_all(root: &Path, visit: &mut dyn FnMut(&Path)) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(iter) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in iter.flatten() {
            let path = entry.path();
            visit(&path);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                stack.push(path);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn flags_world_writable_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("inside");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();

        let mut report = ProbeReport::new(PROBE);
        check_world_writable(tmp.path(), &mut report);
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("world-writable")));
    }

    #[test]
    fn ignores_safe_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("inside");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();

        let mut report = ProbeReport::new(PROBE);
        check_world_writable(tmp.path(), &mut report);
        assert!(!report
            .findings
            .iter()
            .any(|f| f.title.contains("world-writable")));
    }

    #[test]
    fn flags_outward_symlink() {
        let outer = tempfile::tempdir().unwrap();
        let inner = tempfile::tempdir().unwrap();
        let link = inner.path().join("escape");
        std::os::unix::fs::symlink(outer.path(), &link).unwrap();

        let mut report = ProbeReport::new(PROBE);
        check_symlinks_outward(
            inner.path(),
            &inner.path().canonicalize().unwrap(),
            &mut report,
        );
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("escapes project root")));
    }
}
