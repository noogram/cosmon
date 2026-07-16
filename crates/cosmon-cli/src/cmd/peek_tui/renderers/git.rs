// SPDX-License-Identifier: AGPL-3.0-only

//! Renders `git status --short --branch` + `git diff --stat` for the
//! worker's worktree. Falls back to the project root when the worktree
//! is absent (pending / collapsed molecules).

use std::fmt::Write as _;
use std::process::Command;

use ansi_to_tui::IntoText;
use ratatui::text::Text;

use super::{DetailCtx, DetailRenderer};

pub(crate) struct GitRenderer;

impl DetailRenderer for GitRenderer {
    fn keys(&self) -> &'static [char] {
        &['g']
    }

    fn label(&self) -> &'static str {
        "git"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        // Worktree is conventionally `<repo>/.worktrees/<mol-id>`. Fall back
        // to the project_root if the worktree path does not exist.
        let Some(sd) = ctx.state_dir else {
            return Text::raw("<no state dir>");
        };
        let project_root = sd.parent().and_then(std::path::Path::parent);
        let Some(project_root) = project_root else {
            return Text::raw("<no project root>");
        };
        let worktree = project_root.join(".worktrees").join(&ctx.row.mol_id);
        let dir = if worktree.is_dir() {
            worktree
        } else {
            project_root.to_path_buf()
        };
        let status = Command::new("git")
            .args(["-C"])
            .arg(&dir)
            .args(["-c", "color.ui=always"])
            .args(["status", "--short", "--branch"])
            .output();
        let stat = Command::new("git")
            .args(["-C"])
            .arg(&dir)
            .args(["-c", "color.ui=always"])
            .args(["diff", "--stat", "--color=always"])
            .output();
        let mut out = String::new();
        let _ = writeln!(out, "# {}\n", dir.display());
        match status {
            Ok(o) if o.status.success() => {
                out.push_str("## status\n");
                out.push_str(&String::from_utf8_lossy(&o.stdout));
                out.push('\n');
            }
            Ok(o) => {
                let _ = writeln!(
                    out,
                    "<git status failed: {}>",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                let _ = writeln!(out, "<git status error: {e}>");
            }
        }
        match stat {
            Ok(o) if o.status.success() => {
                out.push_str("\n## diff --stat\n");
                out.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            Ok(o) => {
                let _ = writeln!(
                    out,
                    "<git diff failed: {}>",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                let _ = writeln!(out, "<git diff error: {e}>");
            }
        }
        out.as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(out))
    }
}
