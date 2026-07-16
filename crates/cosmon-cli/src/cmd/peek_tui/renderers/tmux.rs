// SPDX-License-Identifier: AGPL-3.0-only

//! Live tmux pane capture — the "eye of Sauron" view. Non-intrusive: runs
//! `tmux capture-pane -p -e` only for the currently-selected row, and
//! routes the escape-coded output through `ansi-to-tui` so the worker's
//! colors survive the trip into the TUI.

use ansi_to_tui::IntoText;
use ratatui::text::Text;

use super::{read_artifact_file, DetailCtx, DetailRenderer};

pub(crate) struct TmuxRenderer;

impl DetailRenderer for TmuxRenderer {
    fn keys(&self) -> &'static [char] {
        &['p', ' ']
    }

    fn label(&self) -> &'static str {
        "tmux"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let raw = match &ctx.row.session {
            Some(s) => super::super::capture_pane(&ctx.row.socket, s)
                .unwrap_or_else(|e| format!("<capture failed: {e}>")),
            None => {
                // Fallback: show gate-output.log if present (shell gate steps
                // bypass tmux entirely).
                read_artifact_file(
                    ctx.molecule_dir,
                    "gate-output.log",
                    "<no tmux session attached>",
                )
            }
        };
        raw.as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(raw))
    }
}
