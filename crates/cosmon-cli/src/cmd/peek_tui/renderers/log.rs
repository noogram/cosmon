// SPDX-License-Identifier: AGPL-3.0-only

//! Renders `log.md` — the worker's append-only progress journal. Preserves
//! ANSI color/style escapes emitted by the worker (compiler output, clippy
//! diagnostics, test runners) by routing the file through `ansi-to-tui`
//! instead of printing the raw escape bytes as literal characters.

use ansi_to_tui::IntoText;
use ratatui::text::Text;

use super::{read_artifact_file, DetailCtx, DetailRenderer};

pub(crate) struct LogRenderer;

impl DetailRenderer for LogRenderer {
    fn keys(&self) -> &'static [char] {
        &['l']
    }

    fn label(&self) -> &'static str {
        "log"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let raw = read_artifact_file(ctx.molecule_dir, "log.md", "<no log.md yet>");
        raw.as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(raw))
    }
}
