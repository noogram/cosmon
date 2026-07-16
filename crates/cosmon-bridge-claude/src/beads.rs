// SPDX-License-Identifier: AGPL-3.0-only

//! Beads CLI wrapper — shell out to `bd` for issue tracking.
//!
//! These functions provide a typed Rust interface over Gas Town's `bd` CLI tool.
//! No native Dolt dependency — all operations go through the `bd` binary.
//!
//! Standalone functions per ADR-COS-001 — no trait indirection.

use std::process::Command;

/// Error type for beads CLI operations.
#[derive(Debug, thiserror::Error)]
pub enum BeadsError {
    /// The `bd` command failed with an error message.
    #[error("bd command failed: {0}")]
    CommandFailed(String),

    /// An I/O error running the `bd` binary.
    #[error("I/O error: {0}")]
    Io(String),

    /// Failed to parse `bd` output.
    #[error("parse error: {0}")]
    ParseError(String),
}

/// Summary of a bead returned by [`list_beads`].
#[derive(Debug, Clone)]
pub struct BeadSummary {
    /// The bead ID (e.g. "cs-abc").
    pub id: String,
    /// The bead title.
    pub title: String,
    /// The bead status.
    pub status: String,
}

/// Run a `bd` command with arguments and return stdout on success.
fn bd_cmd(args: &[&str]) -> Result<String, BeadsError> {
    let output = Command::new("bd")
        .args(args)
        .output()
        .map_err(|e| BeadsError::Io(format!("failed to run bd: {e}")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(BeadsError::CommandFailed(format!(
            "exit {}: {}{}",
            output.status.code().unwrap_or(-1),
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stdout.trim())
            }
        )))
    }
}

/// Create a new bead via `bd create`.
///
/// Returns the bead ID on success (parsed from `bd` output).
///
/// # Errors
///
/// Returns [`BeadsError`] if the `bd create` command fails.
pub fn create_bead(
    title: &str,
    bead_type: &str,
    priority: Option<u8>,
) -> Result<String, BeadsError> {
    let mut args = vec!["create", "--title", title, "--type", bead_type];

    let priority_str;
    if let Some(p) = priority {
        priority_str = p.to_string();
        args.push("--priority");
        args.push(&priority_str);
    }

    let output = bd_cmd(&args)?;

    // bd create typically outputs the bead ID on success.
    // Parse the first word that looks like an ID.
    let id = output
        .split_whitespace()
        .find(|w| w.contains('-'))
        .unwrap_or(output.trim())
        .to_owned();

    if id.is_empty() {
        return Err(BeadsError::ParseError(
            "no bead ID found in bd create output".to_owned(),
        ));
    }

    Ok(id)
}

/// Close a bead via `bd close`.
///
/// # Errors
///
/// Returns [`BeadsError`] if the `bd close` command fails.
pub fn close_bead(id: &str, reason: Option<&str>) -> Result<(), BeadsError> {
    let mut args = vec!["close", id];

    let reason_flag;
    if let Some(r) = reason {
        reason_flag = format!("--reason={r}");
        args.push(&reason_flag);
    }

    bd_cmd(&args)?;
    Ok(())
}

/// Update a bead via `bd update`.
///
/// Supports updating status, notes, and design fields.
///
/// # Errors
///
/// Returns [`BeadsError`] if the `bd update` command fails.
pub fn update_bead(
    id: &str,
    status: Option<&str>,
    notes: Option<&str>,
    design: Option<&str>,
) -> Result<(), BeadsError> {
    let mut args = vec!["update", id];

    let status_flag;
    if let Some(s) = status {
        status_flag = format!("--status={s}");
        args.push(&status_flag);
    }

    let notes_flag;
    if let Some(n) = notes {
        notes_flag = format!("--notes={n}");
        args.push(&notes_flag);
    }

    let design_flag;
    if let Some(d) = design {
        design_flag = format!("--design={d}");
        args.push(&design_flag);
    }

    bd_cmd(&args)?;
    Ok(())
}

/// List beads via `bd list`, optionally filtered by status.
///
/// Returns a vector of [`BeadSummary`] parsed from `bd list` output.
///
/// # Errors
///
/// Returns [`BeadsError`] if the `bd list` command fails.
pub fn list_beads(status: Option<&str>) -> Result<Vec<BeadSummary>, BeadsError> {
    let mut args = vec!["list"];

    let status_flag;
    if let Some(s) = status {
        status_flag = format!("--status={s}");
        args.push(&status_flag);
    }

    let output = bd_cmd(&args)?;

    let mut beads = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // bd list output format: "◇ ID · Title   [● Pn · STATUS]"
        // We parse the ID and attempt to extract title and status.
        if let Some(bead) = parse_bead_line(line) {
            beads.push(bead);
        }
    }

    Ok(beads)
}

/// Parse a single line from `bd list` output into a [`BeadSummary`].
fn parse_bead_line(line: &str) -> Option<BeadSummary> {
    // Strip leading decoration (◇, ●, etc.)
    let stripped = line
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != '[')
        .trim();

    // The ID is the first word
    let mut parts = stripped.splitn(2, |c: char| c.is_whitespace() || c == '·');
    let id = parts.next()?.trim().to_owned();

    if id.is_empty() {
        return None;
    }

    // Rest contains title and status
    let rest = parts.next().unwrap_or("").trim();

    // Try to extract status from brackets at end: [● Pn · STATUS]
    let (title, status) = if let Some(bracket_start) = rest.rfind('[') {
        let title = rest[..bracket_start].trim().trim_matches('·').trim();
        let status_part = &rest[bracket_start..];
        let status = status_part
            .trim_matches(|c: char| c == '[' || c == ']')
            .split('·')
            .next_back()
            .unwrap_or("")
            .trim()
            .to_owned();
        (title.to_owned(), status)
    } else {
        (rest.trim_matches('·').trim().to_owned(), String::new())
    };

    Some(BeadSummary { id, title, status })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bead_line_typical() {
        let line = "◇ cs-ryn · claude-spawn-kill: spawn/kill/check_alive   [● P1 · HOOKED]";
        let bead = parse_bead_line(line).expect("should parse");
        assert_eq!(bead.id, "cs-ryn");
        assert!(bead.title.contains("claude-spawn-kill"));
        assert_eq!(bead.status, "HOOKED");
    }

    #[test]
    fn test_parse_bead_line_minimal() {
        let line = "cs-abc · Some title";
        let bead = parse_bead_line(line).expect("should parse");
        assert_eq!(bead.id, "cs-abc");
    }

    #[test]
    fn test_parse_bead_line_empty() {
        assert!(parse_bead_line("").is_none());
        assert!(parse_bead_line("   ").is_none());
    }
}
