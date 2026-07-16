// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor mcp` — audit registered MCP servers via the neurion registry.
//!
//! Each MCP server is a process that speaks tool-calls to the AI session —
//! a tasty target for an attacker who wants to pivot from "model" to
//! "operator shell". This probe queries the neurion inventory (read-only)
//! and flags the following hazards per row:
//!
//! - **Missing binary** — `command` does not resolve to an existing file.
//!   The MCP spawn would fail silently, masking shadow-binary swaps.
//! - **Inlined token in args** — the JSON `args` array contains a string
//!   that matches our secret-pattern set (see `super::leaks::CONTENT_PATTERNS`
//!   via a local mirror). A token on the command line is visible to
//!   every process on the host via `ps`.
//! - **Config file world-readable** — a file in `config_files` has
//!   permission bits that allow non-owner reads. Those configs typically
//!   contain the credentials that the MCP server uses upstream.
//!
//! If the neurion registry is absent, the probe emits a `Warning`
//! (the audit is not *enforceable* without neurion) but still exits
//! zero so that fresh machines or CI images don't fail spuriously.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::findings::{Finding, ProbeReport, Severity};

const PROBE: &str = "mcp";

/// Content patterns we treat as "this string looks like a secret". Kept
/// in sync with the leaks probe — we deliberately duplicate a small
/// table rather than expose it publicly across probe boundaries.
const TOKEN_PATTERNS: &[&str] = &[
    "ghp_",
    "github_pat_",
    "gho_",
    "ghu_",
    "ghs_",
    "sk-ant-",
    "sk-proj-",
    "xoxb-",
    "xoxp-",
    "AIza",
    "AKIA",
    "ASIA",
];

/// Arguments for `cs doctor mcp`.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Override the path to the service registry database.
    ///
    /// Defaults to the platform data dir used by the service registry.
    #[arg(long)]
    pub registry: Option<PathBuf>,
}

/// Run the MCP server audit.
///
/// `registry_override` lets tests point at a fixture database.
///
/// # Errors
/// Returns an error if the registry database opens but its schema is
/// unusable (missing expected columns). A missing database becomes a
/// `Warning` finding, not an error.
pub fn scan(registry_override: Option<&Path>) -> anyhow::Result<ProbeReport> {
    let mut report = ProbeReport::new(PROBE);
    let db_path = match registry_override {
        Some(p) => p.to_path_buf(),
        None => default_registry_path()?,
    };

    if !db_path.exists() {
        report.findings.push(
            Finding::new(
                PROBE,
                Severity::Warning,
                "neurion registry not found — MCP audit skipped",
            )
            .with_path(&db_path)
            .with_remediation(
                "Install or start the neurion MCP, or pass --registry <path>.".to_owned(),
            ),
        );
        return Ok(report);
    }

    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| anyhow::anyhow!("cannot open neurion registry at {}: {e}", db_path.display()))?;

    let rows = load_mcp_rows(&conn)?;
    for row in rows {
        report.scanned += 1;
        audit_row(&row, &mut report);
    }
    Ok(report)
}

/// CLI entry point for `cs doctor mcp`.
///
/// # Errors
/// Returns an error if the registry is present but unreadable.
pub fn run(ctx: &super::Context, args: &Args) -> anyhow::Result<()> {
    let report = scan(args.registry.as_deref())?;
    super::emit_report_and_exit(ctx, &[report])
}

#[derive(Debug)]
pub(super) struct McpRow {
    pub name: String,
    pub command: String,
    pub args_json: String,
    pub config_files_json: String,
}

fn load_mcp_rows(conn: &Connection) -> anyhow::Result<Vec<McpRow>> {
    let mut stmt = conn
        .prepare("SELECT name, command, args, config_files FROM mcp_servers")
        .map_err(|e| anyhow::anyhow!("mcp_servers table unreadable: {e}"))?;
    let iter = stmt
        .query_map([], |row| {
            Ok(McpRow {
                name: row.get(0)?,
                command: row.get(1)?,
                args_json: row
                    .get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "[]".into()),
                config_files_json: row
                    .get::<_, Option<String>>(3)?
                    .unwrap_or_else(|| "[]".into()),
            })
        })
        .map_err(|e| anyhow::anyhow!("mcp_servers query failed: {e}"))?;
    let mut out = Vec::new();
    for r in iter {
        out.push(r.map_err(|e| anyhow::anyhow!("row decode: {e}"))?);
    }
    Ok(out)
}

fn audit_row(row: &McpRow, report: &mut ProbeReport) {
    // 1. Command binary must exist and be a regular file.
    if !row.command.is_empty() {
        let cmd_path = resolve_command(&row.command);
        match cmd_path {
            Some(p) if p.exists() => {}
            Some(p) => {
                report.findings.push(
                    Finding::new(
                        PROBE,
                        Severity::Warning,
                        format!("MCP server `{}`: command not found on disk", row.name),
                    )
                    .with_path(&p)
                    .with_remediation(
                        "Re-install the MCP or remove the stale registry row.".to_owned(),
                    ),
                );
            }
            None => {
                report.findings.push(Finding::new(
                    PROBE,
                    Severity::Warning,
                    format!(
                        "MCP server `{}`: command `{}` not on PATH",
                        row.name, row.command
                    ),
                ));
            }
        }
    }

    // 2. Args must not inline obvious secret patterns.
    if let Ok(args) = serde_json::from_str::<Vec<String>>(&row.args_json) {
        for (i, arg) in args.iter().enumerate() {
            for pat in TOKEN_PATTERNS {
                if arg.contains(pat) {
                    report.findings.push(
                        Finding::new(
                            PROBE,
                            Severity::Error,
                            format!(
                                "MCP server `{}` has inline-looking secret in args[{i}] (matched `{pat}`)",
                                row.name
                            ),
                        )
                        .with_remediation(
                            "Move the token to an env var or the MCP's config file (chmod 0600)."
                                .to_owned(),
                        ),
                    );
                    break;
                }
            }
        }
    }

    // 3. config_files must be readable only by owner.
    if let Ok(files) = serde_json::from_str::<Vec<String>>(&row.config_files_json) {
        for f in files {
            let path = expand_tilde(&f);
            check_config_perms(&row.name, &path, report);
        }
    }
}

fn resolve_command(cmd: &str) -> Option<PathBuf> {
    let p = PathBuf::from(cmd);
    if p.is_absolute() || cmd.contains('/') {
        return Some(p);
    }
    // Walk PATH looking for an executable.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(cmd);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(s)
}

#[cfg(unix)]
fn check_config_perms(mcp_name: &str, path: &Path, report: &mut ProbeReport) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::metadata(path) else {
        return; // missing file is the MCP's problem, not this probe's
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o044 != 0 {
        report.findings.push(
            Finding::new(
                PROBE,
                Severity::Warning,
                format!("MCP `{mcp_name}` config readable by group/other (mode 0{mode:03o})"),
            )
            .with_path(path)
            .with_remediation(format!("chmod 0600 {}", path.display())),
        );
    }
}

#[cfg(not(unix))]
fn check_config_perms(_mcp_name: &str, _path: &Path, _report: &mut ProbeReport) {
    // Permission bits are not comparable on non-Unix.
}

fn default_registry_path() -> anyhow::Result<PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join("neurion.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_seeded(path: &Path, seed: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(neurion_core::schema::SCHEMA_SQL)
            .unwrap();
        conn.execute_batch(neurion_core::schema::HYPERGRAPH_SQL)
            .unwrap();
        conn.execute_batch(seed).unwrap();
        drop(conn);
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    #[test]
    fn flags_inline_token_in_args() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("n.db");
        let _ = open_seeded(
            &db,
            "INSERT INTO mcp_servers (name, command, args) VALUES \
             ('weather', 'echo', '[\"--token\", \"ghp_SECRET_ABCDEF\"]');",
        );
        let report = scan(Some(&db)).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.title.contains("weather")));
    }

    #[test]
    fn clean_row_emits_no_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("n.db");
        let _ = open_seeded(
            &db,
            "INSERT INTO mcp_servers (name, command, args) VALUES \
             ('echo', 'echo', '[\"hello\"]');",
        );
        let report = scan(Some(&db)).unwrap();
        assert_eq!(report.count(Severity::Error), 0);
    }

    #[test]
    fn missing_registry_is_warning_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let report = scan(Some(&tmp.path().join("absent.db"))).unwrap();
        assert!(!report.has_errors());
        assert!(report
            .findings
            .iter()
            .any(|f| f.severity == Severity::Warning));
    }
}
