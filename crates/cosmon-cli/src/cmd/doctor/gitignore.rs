// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor gitignore` — does `.cosmon/.gitignore` do what it says?
//!
//! The archive subsystem writes a molecule's durable artifacts under
//! `.cosmon/state/archive/`, and the shipped ignore body re-includes that
//! subtree by negation so the chain of reasoning reaches git history. Two
//! ways that arrangement breaks silently, both observed in issue #60:
//!
//! - The shipped rule was `state/` rather than `state/*`. git does not
//!   descend into an excluded directory, so `!state/archive/` matched
//!   nothing and the file's own comment described behaviour the rules did
//!   not have.
//! - A galaxy's file was hand-rewritten — by an agent, on a user's behalf —
//!   into a chain of rules that ignored and re-included each other. Because
//!   `cs init --upgrade` only replaces bodies it recognises byte for byte,
//!   a customised file is never revisited, broken or not.
//!
//! Neither state announces itself: the archive keeps being written, and the
//! files simply never appear in `git status`. This probe asks real git
//! (`git check-ignore`) and names the rule responsible, so the user learns
//! the state exists rather than discovering it after a `cs done`.
//!
//! Findings are `Severity::Warning` — an un-versioned archive is a loss of
//! provenance, not a broken build, and `cs doctor` must not start failing
//! CI on a galaxy whose operator deliberately keeps the archive local.

use std::path::{Path, PathBuf};

use cosmon_core::config::ProjectConfig;
use cosmon_filestore::project_upgrade::archive_subtree_ignored_rule;

use super::findings::{Finding, ProbeReport, Severity};
use super::Context;

const PROBE: &str = "gitignore";

/// Arguments for `cs doctor gitignore`.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Override the galaxy root (the directory containing `.cosmon/`).
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Run the probe for the galaxy rooted at `root`.
///
/// `archive_enabled` comes from the project's `[archive] enabled`; when the
/// archive is off, an ignored archive subtree is not a defect and the probe
/// says so instead of warning about nothing.
///
/// # Errors
/// Never fails: an unreadable or absent `.cosmon/` becomes an `Info`
/// finding. A probe that aborts teaches the user less than one that reports.
#[allow(clippy::unnecessary_wraps)]
pub fn scan(root: &Path, archive_enabled: bool) -> anyhow::Result<ProbeReport> {
    let mut report = ProbeReport::new(PROBE);
    let cosmon_dir = root.join(".cosmon");
    if !cosmon_dir.is_dir() {
        report.findings.push(
            Finding::new(PROBE, Severity::Info, "no .cosmon/ here — nothing to audit")
                .with_path(&cosmon_dir),
        );
        return Ok(report);
    }
    report.scanned = 1;

    let ignore_path = cosmon_dir.join(".gitignore");
    if !ignore_path.is_file() {
        report.findings.push(
            Finding::new(PROBE, Severity::Warning, "no .cosmon/.gitignore")
                .with_path(&ignore_path)
                .with_remediation("run `cs init --upgrade` to write the cosmon-managed block"),
        );
        return Ok(report);
    }

    let Some(rule) = archive_subtree_ignored_rule(root, &cosmon_dir) else {
        report.findings.push(Finding::new(
            PROBE,
            Severity::Info,
            "archive subtree is tracked — git descends into state/archive/",
        ));
        return Ok(report);
    };

    if archive_enabled {
        report.findings.push(
            Finding::new(
                PROBE,
                Severity::Warning,
                "archive is enabled but .cosmon/.gitignore excludes state/archive/",
            )
            .with_path(&ignore_path)
            .with_detail(format!(
                "git check-ignore -v: {rule}\n\
                 Every molecule's durable artifacts are written and then never \
                 appear in `git status`; a fresh clone of this galaxy carries \
                 none of them."
            ))
            .with_remediation(
                "run `cs init --upgrade`: it rewrites the cosmon-managed block, \
                 or appends one below your own rules, preserving every line you wrote",
            ),
        );
    } else {
        report.findings.push(Finding::new(
            PROBE,
            Severity::Info,
            "archive subtree is ignored, and [archive] enabled = false — consistent",
        ));
    }

    Ok(report)
}

/// Execute `cs doctor gitignore`.
///
/// # Errors
/// Propagates a failure from the report emitter.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let root = match &args.root {
        Some(p) => p.clone(),
        None => galaxy_root(ctx),
    };
    let config_path = super::super::resolve_config_from_context(ctx);
    let enabled = cosmon_filestore::load_project_config(&config_path)
        .unwrap_or_else(|_| ProjectConfig::default())
        .archive
        .enabled;
    let report = scan(&root, enabled)?;
    super::emit_report_and_exit(ctx, std::slice::from_ref(&report))
}

/// The galaxy root — the directory containing the resolved `.cosmon/state/`.
///
/// Falls back to the current directory when the resolved state dir has no
/// two-level ancestry (a flat test layout), which the probe then reports as
/// "no `.cosmon/` here" rather than failing.
fn galaxy_root(ctx: &Context) -> PathBuf {
    let state_dir = ctx.state_dir();
    state_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A galaxy whose ignore body is the current one reports Info, not a
    /// warning — the probe must not cry wolf on a healthy project.
    #[test]
    fn healthy_body_is_reported_as_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q", "."])
            .status()
            .expect("git init");
        let cosmon = root.join(".cosmon");
        std::fs::create_dir_all(&cosmon).expect("mkdir");
        std::fs::write(
            cosmon.join(".gitignore"),
            cosmon_filestore::project_upgrade::COSMON_GITIGNORE_CONTENT,
        )
        .expect("write");

        let report = scan(root, true).expect("scan");
        assert!(!report.has_errors());
        assert_eq!(report.count(Severity::Warning), 0);
    }

    /// The broken body, with the archive on, is named as a warning.
    #[test]
    fn broken_body_is_named_when_archive_is_enabled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q", "."])
            .status()
            .expect("git init");
        let cosmon = root.join(".cosmon");
        std::fs::create_dir_all(&cosmon).expect("mkdir");
        std::fs::write(
            cosmon.join(".gitignore"),
            cosmon_filestore::project_upgrade::LEGACY_BROKEN_NEGATION_GITIGNORE_CONTENT,
        )
        .expect("write");

        let report = scan(root, true).expect("scan");
        assert_eq!(report.count(Severity::Warning), 1);
        let f = &report.findings[0];
        assert!(f.title.contains("excludes state/archive/"));
        assert!(f.detail.as_ref().expect("detail").contains("state/"));

        // Same body, archive off: consistent, not a defect.
        let off = scan(root, false).expect("scan");
        assert_eq!(off.count(Severity::Warning), 0);
    }
}
