// SPDX-License-Identifier: AGPL-3.0-only

//! Read-side projection of the `ModelSelected` event (delib-20260704-b476 C3).
//!
//! C2 promoted the per-molecule **model** attribution from a
//! `model-selection.json` sidecar onto a typed
//! [`EventV2::ModelSelected`](cosmon_core::event_v2::EventV2::ModelSelected)
//! line on `events.jsonl`. C3 is the observability half: this module folds
//! that log into a per-molecule [`ModelAttribution`] so `cs ensemble` and
//! `cs observe` can answer *"which model ran for this molecule, and why?"*
//! at a glance — without the operator running a `jq` query by hand.
//!
//! The fold is a pure projection (the DAG / control-plane discipline from
//! `CLAUDE.md`): the events log is authoritative content, the attribution is
//! a derived view. "Latest wins" — a molecule re-tackled with a different
//! `--model` carries the most recent `ModelSelected`, mirroring how the
//! spawn itself is last-writer-wins.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{Envelope, EventV2, ModelSelectionSource};
use cosmon_core::id::MoleculeId;

use crate::event_log::resolve_events_log_path;

/// The model attribution for one molecule, projected from the latest
/// [`EventV2::ModelSelected`](cosmon_core::event_v2::EventV2::ModelSelected)
/// on `events.jsonl`.
///
/// Carries the resolved `(adapter, model, source)` bundle plus the wall-clock
/// time the choice was made. `model` is `None` at the von-neumann floor — no
/// pin, the adapter's own default applies — never a named strong model reached
/// from silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAttribution {
    /// Adapter the model id is scoped to (a model id only has meaning inside
    /// its adapter).
    pub adapter_name: String,
    /// The pinned model id, or `None` at the floor (adapter default applies).
    pub model: Option<String>,
    /// Where the selection came from — the six-level resolution chain.
    pub source: ModelSelectionSource,
    /// Wall-clock time the selection happened (before the spawn probe).
    pub selected_at: DateTime<Utc>,
}

impl ModelAttribution {
    /// Compact model label for a table cell: the pinned model id, or
    /// `"default"` at the floor (nothing pinned one → adapter default).
    ///
    /// The floor renders as the word `default` rather than an empty cell so a
    /// reader distinguishes "explicitly rode the adapter default" from "no
    /// attribution recorded at all" (the latter surfaces as `None` at the
    /// [`ModelAttribution`] level, i.e. no row here).
    #[must_use]
    pub fn model_label(&self) -> &str {
        self.model.as_deref().unwrap_or("default")
    }

    /// Stable kebab/snake slug for the selection source, matching the serde
    /// tag on [`ModelSelectionSource`] (`flag` / `formula_pin` / `env_var` /
    /// `config` / `global_config` / `default`). Machine-stable — safe to
    /// surface on the `--json` wire.
    #[must_use]
    pub fn source_slug(&self) -> &'static str {
        match &self.source {
            ModelSelectionSource::Flag { .. } => "flag",
            ModelSelectionSource::FormulaPin { .. } => "formula_pin",
            ModelSelectionSource::EnvVar { .. } => "env_var",
            ModelSelectionSource::Config { .. } => "config",
            ModelSelectionSource::GlobalConfig { .. } => "global_config",
            ModelSelectionSource::Default { .. } => "default",
            // `ModelSelectionSource` is `#[non_exhaustive]`: a future arm we
            // do not yet know how to label degrades to a stable placeholder
            // rather than failing the read.
            _ => "unknown",
        }
    }

    /// Ultra-compact source badge for a dense table cell (`cs ensemble`):
    /// one or two words, no payload. The full origin lives in
    /// [`source_detail`](Self::source_detail) for the `cs observe` view.
    #[must_use]
    pub fn source_short(&self) -> &'static str {
        match &self.source {
            ModelSelectionSource::Flag { .. } => "--model",
            ModelSelectionSource::FormulaPin { .. } => "formula",
            ModelSelectionSource::EnvVar { .. } => "env",
            ModelSelectionSource::Config { .. } => "config",
            ModelSelectionSource::GlobalConfig { .. } => "global",
            ModelSelectionSource::Default { .. } => "floor",
            _ => "?",
        }
    }

    /// One-line human explanation of *where* the model came from, carrying the
    /// source's payload (the flag value, the formula step, the env var name,
    /// the config path). Rendered on the `cs observe <id>` detail view so an
    /// operator reads the origin without correlating against shell history.
    #[must_use]
    pub fn source_detail(&self) -> String {
        match &self.source {
            ModelSelectionSource::Flag { flag } => format!("--model {flag}"),
            ModelSelectionSource::FormulaPin { formula, step_id } => {
                format!("formula pin ({formula} · step {step_id})")
            }
            ModelSelectionSource::EnvVar { var } => format!("env ${var}"),
            ModelSelectionSource::Config { path, key } => format!("config {key} ({path})"),
            ModelSelectionSource::GlobalConfig { path } => format!("global config ({path})"),
            ModelSelectionSource::Default { fallback_reason } => {
                format!("floor — {fallback_reason}")
            }
            _ => "unknown source".to_owned(),
        }
    }
}

/// The flattened serde tag a `ModelSelected` envelope carries on the wire.
///
/// [`EventV2`](cosmon_core::event_v2::EventV2) is an internally-tagged enum
/// (`#[serde(tag = "type", rename_all = "snake_case")]`) flattened into the
/// [`Envelope`], so every `ModelSelected` line contains this exact substring
/// and no other event kind does. Matching the *full* tag (not the bare
/// `model_selected` token) keeps prose that merely mentions the variant —
/// e.g. a `molecule_completed` reason describing the C2 change — from
/// triggering a needless parse.
const MODEL_SELECTED_TAG: &str = "\"type\":\"model_selected\"";

/// Test whether a raw `events.jsonl` line could be a `ModelSelected` envelope.
///
/// A cheap substring pre-filter run before the (comparatively expensive)
/// serde parse: only lines carrying the [`MODEL_SELECTED_TAG`] survive.
/// Because the writer emits compact JSON (no spaces around `:`) this catches
/// every real envelope; a pretty-printed variant would need re-parsing, but
/// the canonical `events.jsonl` writer never pretty-prints. The filter is an
/// optimisation, never a correctness gate — the typed `match` below is the
/// authority.
fn looks_like_model_selected(line: &str) -> bool {
    line.contains(MODEL_SELECTED_TAG)
}

/// Scan the `ModelSelected` events off `events.jsonl`, calling `visit` for
/// each one in log (append) order.
///
/// The log is an unbounded append-only file (hundreds of MB in a busy
/// galaxy), and both `cs observe` and `cs ensemble` are interactive, so the
/// scan must be cheap. The shape that wins:
///
/// 1. **One whole-file read** into a byte buffer, then a single
///    [`String::from_utf8_lossy`] — for the (always) valid-UTF-8 JSON log
///    this borrows without a second allocation. One big read beats a
///    `BufReader::lines()` walk, which allocates a fresh `String` per line
///    (millions of allocations over a 230 MB log — ~10× slower in practice).
/// 2. **A byte-substring pre-filter** ([`looks_like_model_selected`]) over
///    each `&str` line slice (zero-alloc: `str::lines` yields borrows). Only
///    the handful of lines carrying the tag survive to the serde parse.
///
/// A missing or unreadable log yields no visits (advisory read —
/// trace-not-lock).
fn for_each_model_selected(state_dir: &Path, mut visit: impl FnMut(MoleculeId, ModelAttribution)) {
    let path = resolve_events_log_path(state_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    // `from_utf8_lossy` borrows when the bytes are valid UTF-8 (the JSON log
    // always is) — no second 230 MB allocation on the hot path.
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !looks_like_model_selected(trimmed) {
            continue;
        }
        let Ok(envelope) = Envelope::from_line(trimmed) else {
            continue;
        };
        if let EventV2::ModelSelected {
            mol_id,
            adapter_name,
            model,
            selection_source,
            selected_at,
        } = envelope.event
        {
            visit(
                mol_id,
                ModelAttribution {
                    adapter_name,
                    model,
                    source: selection_source,
                    selected_at,
                },
            );
        }
    }
}

/// Project the latest [`ModelAttribution`] per molecule from `events.jsonl`.
///
/// A single streaming pass over the log — the batch form used by `cs
/// ensemble`, which needs the attribution for every worker's molecule and
/// must not re-read the whole file once per molecule. Later lines overwrite
/// earlier ones for the same molecule id (last-writer-wins), so the map holds
/// each molecule's most recent model selection.
///
/// Returns an empty map when the log is missing or unreadable — the read side
/// is advisory (trace-not-lock, same discipline as the emit side): a molecule
/// with no recorded selection simply carries no attribution.
#[must_use]
pub fn model_selections(state_dir: &Path) -> HashMap<MoleculeId, ModelAttribution> {
    let mut out: HashMap<MoleculeId, ModelAttribution> = HashMap::new();
    for_each_model_selected(state_dir, |mol_id, attr| {
        out.insert(mol_id, attr);
    });
    out
}

/// Project the latest [`ModelAttribution`] for a single molecule.
///
/// The single-molecule form used by `cs observe <id>`. Streams the log and
/// keeps the last `ModelSelected` whose `mol_id` matches — `None` when the
/// molecule has no recorded model selection (a legacy or never-tackled
/// molecule).
#[must_use]
pub fn latest_model_selection(state_dir: &Path, mol_id: &MoleculeId) -> Option<ModelAttribution> {
    let mut latest = None;
    for_each_model_selected(state_dir, |candidate, attr| {
        if &candidate == mol_id {
            latest = Some(attr);
        }
    });
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::worker_spawn::emit_model_selected;
    use tempfile::tempdir;

    fn mol(id: &str) -> MoleculeId {
        MoleculeId::new(id).unwrap()
    }

    /// A flag-sourced emission projects back to a `ModelAttribution` carrying
    /// the model id and the `flag` slug — the round-trip C2-writer → C3-reader.
    #[test]
    fn latest_reads_flag_selection() {
        let dir = tempdir().unwrap();
        let m = mol("task-20260705-a408");
        emit_model_selected(
            dir.path(),
            &m,
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        let attr = latest_model_selection(dir.path(), &m).expect("attribution present");
        assert_eq!(attr.adapter_name, "claude");
        assert_eq!(attr.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(attr.model_label(), "claude-opus-4-8");
        assert_eq!(attr.source_slug(), "flag");
        assert_eq!(attr.source_short(), "--model");
        assert_eq!(attr.source_detail(), "--model claude-opus-4-8");
    }

    /// The floor path: no pin → `model` is `None`, the label reads `default`,
    /// and the source is the `default` slug. Silence never names a strong
    /// model.
    #[test]
    fn latest_reads_floor_selection() {
        let dir = tempdir().unwrap();
        let m = mol("task-20260705-a408");
        emit_model_selected(
            dir.path(),
            &m,
            "claude",
            None,
            ModelSelectionSource::Default {
                fallback_reason: "no pin; adapter default applies".to_owned(),
            },
        );
        let attr = latest_model_selection(dir.path(), &m).expect("attribution present");
        assert!(attr.model.is_none());
        assert_eq!(attr.model_label(), "default");
        assert_eq!(attr.source_slug(), "default");
        assert_eq!(attr.source_short(), "floor");
        assert!(attr.source_detail().starts_with("floor — "));
    }

    /// Last-writer-wins: a molecule re-tackled with a different model carries
    /// the most recent selection, not the first.
    #[test]
    fn latest_takes_most_recent_selection() {
        let dir = tempdir().unwrap();
        let m = mol("task-20260705-a408");
        emit_model_selected(
            dir.path(),
            &m,
            "claude",
            Some("claude-haiku-4-5"),
            ModelSelectionSource::Config {
                path: "/x/.cosmon/config.toml".to_owned(),
                key: "adapters.claude.default_model".to_owned(),
            },
        );
        emit_model_selected(
            dir.path(),
            &m,
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        let attr = latest_model_selection(dir.path(), &m).unwrap();
        assert_eq!(attr.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(attr.source_slug(), "flag");
    }

    /// A molecule with no `ModelSelected` event yields `None` — no
    /// attribution row, cleanly distinct from a recorded floor selection.
    #[test]
    fn absent_selection_is_none() {
        let dir = tempdir().unwrap();
        // Emit for one molecule; query a different one.
        emit_model_selected(
            dir.path(),
            &mol("task-20260705-aaaa"),
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        assert!(latest_model_selection(dir.path(), &mol("task-20260705-bbbb")).is_none());
    }

    /// The batch form folds one line per molecule and keeps the latest for
    /// each — the `cs ensemble` primary read.
    #[test]
    fn batch_folds_latest_per_molecule() {
        let dir = tempdir().unwrap();
        let a = mol("task-20260705-aaaa");
        let b = mol("task-20260705-bbbb");
        emit_model_selected(
            dir.path(),
            &a,
            "claude",
            Some("claude-haiku-4-5"),
            ModelSelectionSource::EnvVar {
                var: "COSMON_DEFAULT_MODEL".to_owned(),
            },
        );
        emit_model_selected(
            dir.path(),
            &b,
            "openai",
            None,
            ModelSelectionSource::Default {
                fallback_reason: "no pin".to_owned(),
            },
        );
        emit_model_selected(
            dir.path(),
            &a,
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        let map = model_selections(dir.path());
        assert_eq!(map.len(), 2);
        assert_eq!(map[&a].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(map[&a].source_slug(), "flag");
        assert_eq!(map[&b].model, None);
        assert_eq!(map[&b].source_slug(), "default");
        assert_eq!(map[&b].adapter_name, "openai");
    }

    /// A missing log is not an error — the read side is advisory.
    #[test]
    fn missing_log_yields_empty() {
        let dir = tempdir().unwrap();
        assert!(model_selections(dir.path()).is_empty());
        assert!(latest_model_selection(dir.path(), &mol("task-20260705-a408")).is_none());
    }
}
