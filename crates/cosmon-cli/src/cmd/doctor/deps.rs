// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor deps` — flag unpinned or unsafe dependency declarations.
//!
//! Supply-chain attacks on Cargo packages (tj-actions/changed-files style)
//! succeed when a dependency can silently upgrade to a compromised
//! version. The CLI here does not try to replace `cargo audit` or
//! `cargo deny`; it checks the minimum the operator can enforce at
//! commit time without any network:
//!
//! - `*` as a version — caret-max, no floor at all.
//! - A git dependency without a pinned `rev = "<sha>"` (using `branch`
//!   or `tag` is mutable — an attacker or maintainer can move the ref).
//! - A path dependency escaping the workspace (relative `..` chains).
//!
//! Findings are `Severity::Warning` — the default Cargo caret semver is
//! the realistic convention, and churning it into an error here would be
//! self-defeating. The probe's value is making drift visible.

use std::fs;
use std::path::{Path, PathBuf};

use super::findings::{Finding, ProbeReport, Severity};

const PROBE: &str = "deps";

/// Arguments for `cs doctor deps`.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Override the workspace root.
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Run the unpinned-deps scan. `root` is the workspace root (must contain
/// `Cargo.toml`).
///
/// # Errors
/// Returns an error if the root `Cargo.toml` cannot be parsed.
pub fn scan(root: &Path) -> anyhow::Result<ProbeReport> {
    let mut report = ProbeReport::new(PROBE);
    let manifests = collect_manifests(root)?;
    for manifest in manifests {
        report.scanned += 1;
        if let Err(e) = audit_manifest(root, &manifest, &mut report) {
            report.findings.push(
                Finding::new(PROBE, Severity::Warning, format!("parse error: {e}"))
                    .with_path(&manifest),
            );
        }
    }
    Ok(report)
}

/// CLI entry point for `cs doctor deps`.
///
/// # Errors
/// Returns an error when the project root cannot be resolved.
pub fn run(ctx: &super::Context, args: &Args) -> anyhow::Result<()> {
    let root = match &args.root {
        Some(p) => p.clone(),
        None => super::leaks::git_root(&std::env::current_dir()?)?,
    };
    let report = scan(&root)?;
    super::emit_report_and_exit(ctx, &[report])
}

fn collect_manifests(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let root_toml = root.join("Cargo.toml");
    let mut out = Vec::new();
    if !root_toml.exists() {
        return Err(anyhow::anyhow!(
            "no Cargo.toml at {} — is this a Rust workspace?",
            root.display()
        ));
    }
    out.push(root_toml.clone());

    // Parse workspace members if present and pull their manifests.
    let raw = fs::read_to_string(&root_toml)?;
    let parsed: toml::Value = toml::from_str(&raw)?;
    if let Some(members) = parsed
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        for m in members {
            let Some(pat) = m.as_str() else { continue };
            for path in expand_member(root, pat) {
                let manifest = path.join("Cargo.toml");
                if manifest.exists() && !out.contains(&manifest) {
                    out.push(manifest);
                }
            }
        }
    }
    Ok(out)
}

/// Expand a workspace member pattern (literal path, or one trailing `*` glob).
fn expand_member(root: &Path, pat: &str) -> Vec<PathBuf> {
    if let Some(prefix) = pat.strip_suffix("/*") {
        let dir = root.join(prefix);
        let Ok(iter) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        iter.flatten()
            .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    } else {
        vec![root.join(pat)]
    }
}

fn audit_manifest(root: &Path, manifest: &Path, report: &mut ProbeReport) -> anyhow::Result<()> {
    let raw = fs::read_to_string(manifest)?;
    let doc: toml::Value = toml::from_str(&raw)?;

    // Skip the virtual manifest section for workspace.dependencies too — same
    // rules apply to them, since every crate picks up their pinning defaults.
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(tbl) = doc.get(section).and_then(|v| v.as_table()) {
            audit_table(root, manifest, section, tbl, report);
        }
    }
    if let Some(ws) = doc.get("workspace").and_then(|v| v.as_table()) {
        if let Some(tbl) = ws.get("dependencies").and_then(|v| v.as_table()) {
            audit_table(root, manifest, "workspace.dependencies", tbl, report);
        }
    }
    // target-specific tables — e.g., [target.'cfg(unix)'.dependencies]
    if let Some(targets) = doc.get("target").and_then(|v| v.as_table()) {
        for (tgt_name, tgt_val) in targets {
            if let Some(tgt_tbl) = tgt_val.as_table() {
                for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(tbl) = tgt_tbl.get(section).and_then(|v| v.as_table()) {
                        audit_table(
                            root,
                            manifest,
                            &format!("target.{tgt_name}.{section}"),
                            tbl,
                            report,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn audit_table(
    root: &Path,
    manifest: &Path,
    section: &str,
    tbl: &toml::value::Table,
    report: &mut ProbeReport,
) {
    for (name, spec) in tbl {
        audit_entry(root, manifest, section, name, spec, report);
    }
}

fn audit_entry(
    root: &Path,
    manifest: &Path,
    section: &str,
    name: &str,
    spec: &toml::Value,
    report: &mut ProbeReport,
) {
    match spec {
        toml::Value::String(v) => {
            if v.trim() == "*" {
                push_wildcard(manifest, section, name, report);
            }
        }
        toml::Value::Table(t) => {
            // Check git deps: must pin with rev.
            if let Some(git) = t.get("git").and_then(|v| v.as_str()) {
                let has_rev = t.get("rev").and_then(|v| v.as_str()).is_some();
                if !has_rev {
                    report.findings.push(
                        Finding::new(
                            PROBE,
                            Severity::Warning,
                            format!(
                                "git dependency `{name}` in [{section}] is not pinned to a commit"
                            ),
                        )
                        .with_path(manifest)
                        .with_detail(format!("git = \"{git}\""))
                        .with_remediation(
                            "Add `rev = \"<commit-sha>\"` — branch/tag refs are mutable."
                                .to_owned(),
                        ),
                    );
                }
            }
            // Check wildcard version strings.
            if let Some(v) = t.get("version").and_then(|v| v.as_str()) {
                if v.trim() == "*" {
                    push_wildcard(manifest, section, name, report);
                }
            } else if t.get("git").is_none()
                && t.get("path").is_none()
                && t.get("workspace").is_none_or(|w| w.as_bool() != Some(true))
            {
                report.findings.push(
                    Finding::new(
                        PROBE,
                        Severity::Warning,
                        format!(
                            "dependency `{name}` in [{section}] has no version/workspace/path/git"
                        ),
                    )
                    .with_path(manifest)
                    .with_remediation(
                        "Add an explicit version = \"x.y\" or workspace = true.".to_owned(),
                    ),
                );
            }
            // Check path deps that escape the workspace root.
            if let Some(path_str) = t.get("path").and_then(|v| v.as_str()) {
                let resolved = manifest
                    .parent()
                    .map_or_else(|| PathBuf::from(path_str), |p| p.join(path_str));
                let canonical = resolved.canonicalize().unwrap_or(resolved);
                let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
                if !canonical.starts_with(&root_canon) {
                    report.findings.push(
                        Finding::new(
                            PROBE,
                            Severity::Warning,
                            format!("path dependency `{name}` escapes workspace root: {path_str}"),
                        )
                        .with_path(manifest)
                        .with_remediation(
                            "Vendor the dep in-tree or publish it as a crate.".to_owned(),
                        ),
                    );
                }
            }
        }
        _ => {}
    }
}

fn push_wildcard(manifest: &Path, section: &str, name: &str, report: &mut ProbeReport) {
    report.findings.push(
        Finding::new(
            PROBE,
            Severity::Warning,
            format!("dependency `{name}` pinned with `*` in [{section}]"),
        )
        .with_path(manifest)
        .with_remediation("Replace `*` with a caret/tilde range or an exact version.".to_owned()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, toml_text: &str) -> PathBuf {
        let path = dir.join("Cargo.toml");
        fs::write(&path, toml_text).unwrap();
        path
    }

    #[test]
    fn flags_wildcard_version() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
bad = "*"
"#,
        );
        let mut report = ProbeReport::new(PROBE);
        audit_manifest(tmp.path(), &manifest, &mut report).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("`bad`") && f.title.contains("`*`")));
    }

    #[test]
    fn flags_unpinned_git() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
sneaky = { git = "https://example.com/s.git", branch = "main" }
"#,
        );
        let mut report = ProbeReport::new(PROBE);
        audit_manifest(tmp.path(), &manifest, &mut report).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("sneaky") && f.title.contains("not pinned")));
    }

    #[test]
    fn accepts_pinned_git() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
ok = { git = "https://example.com/s.git", rev = "abc1234" }
"#,
        );
        let mut report = ProbeReport::new(PROBE);
        audit_manifest(tmp.path(), &manifest, &mut report).unwrap();
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn flags_dep_without_version_or_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
wanderer = { features = ["a"] }
"#,
        );
        let mut report = ProbeReport::new(PROBE);
        audit_manifest(tmp.path(), &manifest, &mut report).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("wanderer") && f.title.contains("no version")));
    }

    #[test]
    fn accepts_workspace_dep() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = write_manifest(
            tmp.path(),
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
shared = { workspace = true, features = ["a"] }
"#,
        );
        let mut report = ProbeReport::new(PROBE);
        audit_manifest(tmp.path(), &manifest, &mut report).unwrap();
        assert_eq!(report.findings.len(), 0);
    }
}
