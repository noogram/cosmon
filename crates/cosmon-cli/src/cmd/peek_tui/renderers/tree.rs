// SPDX-License-Identifier: AGPL-3.0-only

//! DAG navigation pane — upstream blockers and downstream dependents of
//! the selected molecule. Reads directly from the fleet's state store so
//! the view reflects on-disk truth, not the cached `RowView`.

use std::fmt::Write as _;

use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};
use ratatui::text::Text;

use super::{DetailCtx, DetailRenderer};

pub(crate) struct TreeRenderer;

impl DetailRenderer for TreeRenderer {
    fn keys(&self) -> &'static [char] {
        // `t` is reserved for the tackle action keystroke (delib-20260423-becf,
        // task-20260423-16ad). The tree detail pane moves to the shifted
        // letter so `t` can call `cs tackle <selected>`.
        &['T']
    }

    fn label(&self) -> &'static str {
        "tree"
    }

    fn render(&self, ctx: &DetailCtx<'_>) -> Text<'static> {
        let Some(sd) = ctx.state_dir else {
            return Text::raw("<no state dir>");
        };
        let Ok(mid) = cosmon_core::id::MoleculeId::new(ctx.row.mol_id.clone()) else {
            return Text::raw("<invalid molecule id>");
        };
        let store = FileStore::new(sd);

        let mut out = String::new();
        let _ = writeln!(out, "# {} [{}]", ctx.row.mol_id, ctx.row.status);
        if let Some(topic) = &ctx.row.topic {
            let _ = writeln!(out, "{topic}");
        }
        out.push('\n');

        out.push_str("## ⬆ upstream — blocks this\n");
        let upstream: Vec<_> = store
            .load_molecule(&mid)
            .map(|m| m.blocked_by().into_iter().cloned().collect())
            .unwrap_or_default();
        if upstream.is_empty() {
            out.push_str("(none)\n");
        } else {
            for up in &upstream {
                let (status, topic) = match store.load_molecule(up) {
                    Ok(m) => (
                        format!("{:?}", m.status).to_lowercase(),
                        m.display_topic()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    ),
                    Err(_) => ("?".into(), String::new()),
                };
                let _ = writeln!(out, "• {up} [{status}] {topic}");
            }
        }

        out.push_str("\n## ⬇ downstream — blocked by this\n");
        let mut downs: Vec<(String, String, String)> = Vec::new();
        if let Ok(all) = store.list_molecules(&MoleculeFilter::default()) {
            for m in all {
                if m.blocked_by().contains(&&mid) {
                    let topic = m
                        .display_topic()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    downs.push((
                        m.id.to_string(),
                        format!("{:?}", m.status).to_lowercase(),
                        topic,
                    ));
                }
            }
        }
        if downs.is_empty() {
            out.push_str("(none)\n");
        } else {
            downs.sort_by(|a, b| a.0.cmp(&b.0));
            for (id, status, topic) in &downs {
                let _ = writeln!(out, "• {id} [{status}] {topic}");
            }
        }
        Text::raw(out)
    }
}
