// SPDX-License-Identifier: AGPL-3.0-only

//! Read-side projection of the `ModelSelected` event (delib-20260704-b476 C3).
//!
//! C2 promoted the per-molecule **model** attribution from a
//! `model-selection.json` sidecar onto a typed
//! `EventV2::ModelSelected`
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
use cosmon_core::adapter_attribution::AdapterAttribution;
use cosmon_core::event_v2::{Envelope, EventV2, ModelSelectionSource};
use cosmon_core::id::MoleculeId;

use crate::event_log::resolve_events_log_path;

/// The model attribution for one molecule, projected from the latest
/// `EventV2::ModelSelected`
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

/// The distinct adapter names that ran a molecule across its whole life, in
/// first-seen (append) order.
///
/// A molecule that is resumed or handed off (delib-20260717-194b, feynman Q2)
/// can carry more than one `ModelSelected` line with *different* adapters —
/// `claude` on the first tackle, `codex` after a handoff. This surfaces the
/// full set so the caller can decide whether the dispatch is *unambiguous*
/// (exactly one adapter turned the crank) rather than trusting the
/// last-writer-wins projection of [`latest_model_selection`], which would
/// silently pick the most recent and dress a guess as a fact.
///
/// Empty when the molecule has no recorded selection (missing / unreadable
/// log, or never tackled) — advisory read, trace-not-lock.
#[must_use]
pub fn distinct_adapter_names(state_dir: &Path, mol_id: &MoleculeId) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for_each_model_selected(state_dir, |candidate, attr| {
        if &candidate == mol_id {
            let name = attr.adapter_name.trim().to_owned();
            if !name.is_empty() && !seen.iter().any(|s| s == &name) {
                seen.push(name);
            }
        }
    });
    seen
}

/// The outcome of folding a molecule's recorded adapters into the single
/// honest witness the `Co-Authored-By` adapter trailer may credit.
///
/// The trailer answers *"who turned the crank?"* (delib-20260717-194b,
/// feynman Q2). It is a provenance stamp folded from the durable log, never a
/// guess — so it is emitted only on an **unambiguous** witness and dropped in
/// every other case. Dropping costs nothing (no credit is falsely withheld
/// from a person), which is exactly what licenses the aggressive drop rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterFold {
    /// Exactly one distinct adapter ran the molecule — emit it as the second
    /// co-author.
    Single(String),
    /// No adapter recorded (missing / empty / unreadable log) — drop the
    /// adapter trailer. The maker trailer, if any, still rides.
    Absent,
    /// More than one distinct adapter across the molecule's life (resume /
    /// handoff) — ambiguous. Drop the adapter trailer rather than pick
    /// last-writer or list both; carries the witnessed set so the caller can
    /// warn instead of failing silently.
    Ambiguous(Vec<String>),
}

/// Fold a set of distinct adapter names into the [`AdapterFold`] verdict.
///
/// The pure core of the folding rule, split out from I/O so it is unit-testable
/// against a hand-built witness set (delib-20260717-194b, knuth U6): exactly
/// one → [`AdapterFold::Single`]; none → [`AdapterFold::Absent`]; two or more →
/// [`AdapterFold::Ambiguous`]. The input is expected to already be
/// de-duplicated and non-empty-trimmed (as [`distinct_adapter_names`]
/// produces), so the count *is* the verdict.
#[must_use]
pub fn fold_adapter(distinct: &[String]) -> AdapterFold {
    match distinct {
        [] => AdapterFold::Absent,
        [only] => AdapterFold::Single(only.clone()),
        many => AdapterFold::Ambiguous(many.to_vec()),
    }
}

/// Fold a molecule's recorded adapters (streamed from the log) into the single
/// [`AdapterFold`] verdict the attribution trailer builds against.
///
/// Convenience composition of [`distinct_adapter_names`] and [`fold_adapter`]
/// so `cs done` has one call to reach for.
#[must_use]
pub fn folded_adapter(state_dir: &Path, mol_id: &MoleculeId) -> AdapterFold {
    fold_adapter(&distinct_adapter_names(state_dir, mol_id))
}

/// Project the full **specified + realized** attribution for one molecule —
/// what was pinned *and* what actually answered.
///
/// # Why this exists beside [`latest_model_selection`]
///
/// [`ModelAttribution`] folds `ModelSelected` alone, so it can only ever report
/// the **pin**. That was the whole of what `cs observe` showed. When a seat is
/// re-pointed at a sibling model mid-run — a stalled provider switched by hand
/// from inside the pane — the pin keeps naming the model that was *asked for*
/// while a different one answers, and nothing on the operator's primary read
/// verb says so (measured on `converge-20260727-a302`: the roster reported
/// `gpt-5.6-sol` for a seat running `gpt-5.6-terra`).
///
/// [`AdapterAttribution`] already folds both axes honestly, including the
/// attempt-scoping that stops a dead predecessor's observation from
/// contaminating a re-tackle. This is the read-side seam that hands it to a
/// surface, so `cs observe` renders the same `pin~>realized` drift the
/// `cs peek` TUI does instead of a pin the operator has no reason to doubt.
///
/// Unlike the `ModelSelected`-only scan above, the realized axis needs the
/// molecule's events **in order and in full** (attempt boundaries reset the
/// accumulator), so this parses every line belonging to `mol_id` rather than
/// pre-filtering on one tag. Returns `None` when the log is missing,
/// unreadable, or carries nothing for this molecule — advisory read,
/// trace-not-lock, same as its siblings.
#[must_use]
pub fn realized_attribution(state_dir: &Path, mol_id: &MoleculeId) -> Option<AdapterAttribution> {
    let path = resolve_events_log_path(state_dir);
    let bytes = std::fs::read(&path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // The molecule id appears verbatim in every envelope that concerns it, so a
    // substring pre-filter still skips the overwhelming majority of a large
    // log's lines before any serde work — without narrowing to a single event
    // kind, which the ordered fold cannot tolerate.
    let needle = mol_id.as_str();
    let events: Vec<EventV2> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.contains(needle))
        .filter_map(|line| Envelope::from_line(line).ok())
        .map(|envelope| envelope.event)
        .filter(|event| event.molecule_id().is_some_and(|id| id == mol_id))
        .collect();
    if events.is_empty() {
        return None;
    }
    Some(AdapterAttribution::fold(&events))
}

/// The **algorithmic provenance** of a molecule's current attempt
/// (task-20260729-7dd4) — what is known about the *method* that produced its
/// conclusions, beyond the name of the model.
///
/// The sibling read of [`realized_attribution`], folded from the same
/// `events.jsonl` slice in the same pass, so the record can never describe a
/// different attempt than the realized id shown beside it.
///
/// Two distinct `None`s, and a caller must not conflate them: no events at all
/// (nothing was ever dispatched) and no in-scope observation carrying a record
/// (a pre-task-20260729-7dd4 line, or an emitter that never built one). Neither
/// is a *disclosure* — the disclosures live inside the record, where each field
/// names the reason it is empty.
#[must_use]
pub fn realized_provenance(
    state_dir: &Path,
    mol_id: &MoleculeId,
) -> Option<cosmon_core::algorithmic_provenance::AlgorithmicProvenance> {
    let path = resolve_events_log_path(state_dir);
    let bytes = std::fs::read(&path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let needle = mol_id.as_str();
    let events: Vec<EventV2> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.contains(needle))
        .filter_map(|line| Envelope::from_line(line).ok())
        .map(|envelope| envelope.event)
        .filter(|event| event.molecule_id().is_some_and(|id| id == mol_id))
        .collect();
    AdapterAttribution::fold_with_provenance(&events).1
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

    // ---- Adapter folding rule (delib-20260717-194b, F6 / knuth U6) ---------

    /// The pure fold: an empty witness set drops the adapter trailer.
    #[test]
    fn fold_empty_is_absent() {
        assert_eq!(fold_adapter(&[]), AdapterFold::Absent);
    }

    /// Exactly one distinct adapter is the honest, unambiguous witness.
    #[test]
    fn fold_single_is_that_adapter() {
        assert_eq!(
            fold_adapter(&["claude".to_owned()]),
            AdapterFold::Single("claude".to_owned())
        );
    }

    /// Two distinct adapters (resume / handoff) is ambiguous — drop, don't
    /// guess. The witnessed set rides so the caller can warn.
    #[test]
    fn fold_two_distinct_is_ambiguous() {
        let names = vec!["claude".to_owned(), "codex".to_owned()];
        assert_eq!(fold_adapter(&names), AdapterFold::Ambiguous(names));
    }

    /// A molecule with a single recorded adapter folds to `Single`, even when
    /// the same adapter is recorded twice (re-tackle with the same crank).
    #[test]
    fn folded_adapter_single_across_retackle() {
        let dir = tempdir().unwrap();
        let m = mol("task-20260717-a754");
        for model in ["claude-haiku-4-5", "claude-opus-4-8"] {
            emit_model_selected(
                dir.path(),
                &m,
                "claude",
                Some(model),
                ModelSelectionSource::Flag {
                    flag: model.to_owned(),
                },
            );
        }
        assert_eq!(distinct_adapter_names(dir.path(), &m), vec!["claude"]);
        assert_eq!(
            folded_adapter(dir.path(), &m),
            AdapterFold::Single("claude".to_owned())
        );
    }

    /// A handoff — `claude` then `codex` — is ambiguous, so the adapter
    /// trailer is dropped rather than crediting last-writer-wins.
    #[test]
    fn folded_adapter_ambiguous_across_handoff() {
        let dir = tempdir().unwrap();
        let m = mol("task-20260717-a754");
        emit_model_selected(
            dir.path(),
            &m,
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        emit_model_selected(
            dir.path(),
            &m,
            "codex",
            None,
            ModelSelectionSource::Default {
                fallback_reason: "handoff".to_owned(),
            },
        );
        assert_eq!(
            distinct_adapter_names(dir.path(), &m),
            vec!["claude".to_owned(), "codex".to_owned()]
        );
        assert!(matches!(
            folded_adapter(dir.path(), &m),
            AdapterFold::Ambiguous(_)
        ));
    }

    /// No recorded selection folds to `Absent` — the adapter trailer is
    /// dropped, distinct from an ambiguous handoff.
    #[test]
    fn folded_adapter_absent_when_unrecorded() {
        let dir = tempdir().unwrap();
        assert_eq!(
            folded_adapter(dir.path(), &mol("task-20260717-zzzz")),
            AdapterFold::Absent
        );
    }

    // ── realized attribution: the pin is not what answered ───────────────

    /// The measured incident, replayed: a committee seat is pinned to
    /// `gpt-5.6-sol`, its provider refuses mid-review, the operator switches it
    /// to `gpt-5.6-terra` from inside the pane — and the roster keeps reporting
    /// the pin. [`latest_model_selection`] is *structurally incapable* of
    /// noticing (it folds `ModelSelected` only); [`realized_attribution`] is
    /// the seam that does.
    #[test]
    fn realized_attribution_surfaces_a_seat_that_answered_as_another_model() {
        use cosmon_core::event_v2::{AdapterSelectionSource, LoopOwnershipTag};
        use cosmon_core::id::WorkerId;
        use cosmon_core::model_realization::{ModelId, ModelObservationSource};

        use crate::event_log::{emit_one, resolve_events_log_path};
        use crate::events::worker_spawn::emit_new_model_observations;
        use cosmon_core::algorithmic_provenance::AlgorithmicProvenance;

        let dir = tempdir().unwrap();
        let m = mol("cmbverify-20260728-8e2a");
        let log = resolve_events_log_path(dir.path());
        let worker = WorkerId::new("worker-seat-codex").unwrap();

        emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: m.clone(),
                adapter_name: "codex".to_owned(),
                selected_at: Utc::now(),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "codex".to_owned(),
                },
                role_hint: None,
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();
        emit_one(
            &log,
            EventV2::WorkerSpawned {
                worker_id: worker.clone(),
                molecule: Some(m.clone()),
                session_name: "seat".to_owned(),
                role: "refuter".to_owned(),
                adapter_name: "codex".to_owned(),
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();
        emit_model_selected(
            dir.path(),
            &m,
            "codex",
            Some("gpt-5.6-sol"),
            ModelSelectionSource::Flag {
                flag: "gpt-5.6-sol".to_owned(),
            },
        );
        // What actually answered after the in-pane switch.
        emit_new_model_observations(
            dir.path(),
            &m,
            &worker,
            "codex",
            &[ModelId::new("gpt-5.6-terra").unwrap()],
            ModelObservationSource::CodexSessionMeta,
            &AlgorithmicProvenance::adapter_silent("codex"),
        );

        // The pin-only projection — every word of it true, and none of it the
        // model that answered. This is exactly what the roster was showing.
        let pinned = latest_model_selection(dir.path(), &m).expect("selection present");
        assert_eq!(pinned.model_label(), "gpt-5.6-sol");

        // The realized projection names the divergence instead of hiding it.
        let realized = realized_attribution(dir.path(), &m).expect("attribution present");
        assert_eq!(
            realized.realized_drift_display().as_deref(),
            Some("gpt-5.6-terra")
        );
        assert_eq!(
            realized.compact_cell(),
            "codex/gpt-5.6-sol~>gpt-5.6-terra [cli]"
        );
    }

    /// No divergence, no noise: a seat that ran the model it was pinned to
    /// reports no drift. The `~>` glyph means something precisely because it is
    /// absent when specified and realized agree.
    #[test]
    fn realized_attribution_is_silent_when_the_pin_is_what_answered() {
        use cosmon_core::event_v2::{AdapterSelectionSource, LoopOwnershipTag};
        use cosmon_core::id::WorkerId;
        use cosmon_core::model_realization::{ModelId, ModelObservationSource};

        use crate::event_log::{emit_one, resolve_events_log_path};
        use crate::events::worker_spawn::emit_new_model_observations;
        use cosmon_core::algorithmic_provenance::AlgorithmicProvenance;

        let dir = tempdir().unwrap();
        let m = mol("cmbverify-20260728-8e2b");
        let log = resolve_events_log_path(dir.path());
        let worker = WorkerId::new("worker-seat-ok").unwrap();

        emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: m.clone(),
                adapter_name: "codex".to_owned(),
                selected_at: Utc::now(),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "codex".to_owned(),
                },
                role_hint: None,
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();
        emit_one(
            &log,
            EventV2::WorkerSpawned {
                worker_id: worker.clone(),
                molecule: Some(m.clone()),
                session_name: "seat".to_owned(),
                role: "refuter".to_owned(),
                adapter_name: "codex".to_owned(),
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();
        emit_model_selected(
            dir.path(),
            &m,
            "codex",
            Some("gpt-5.6-sol"),
            ModelSelectionSource::Flag {
                flag: "gpt-5.6-sol".to_owned(),
            },
        );
        emit_new_model_observations(
            dir.path(),
            &m,
            &worker,
            "codex",
            &[ModelId::new("gpt-5.6-sol").unwrap()],
            ModelObservationSource::CodexSessionMeta,
            &AlgorithmicProvenance::adapter_silent("codex"),
        );

        let realized = realized_attribution(dir.path(), &m).expect("attribution present");
        assert!(realized.realized_drift_display().is_none());
    }

    /// task-20260729-7dd4 — the algorithmic-provenance record survives the
    /// journal round-trip and comes back beside the realized id it describes.
    ///
    /// This is the end-to-end statement of the primitive: an emitter that can
    /// pin weights, decode and prompt writes them once, and an audit reading
    /// only `events.jsonl` recovers enough to re-run the method.
    #[test]
    fn realized_provenance_round_trips_through_the_journal() {
        use cosmon_core::algorithmic_provenance::{
            AlgorithmicProvenance, DecodingParams, Disclosure, WeightsProvenance,
        };
        use cosmon_core::event_v2::{AdapterSelectionSource, LoopOwnershipTag};
        use cosmon_core::id::WorkerId;
        use cosmon_core::model_realization::{ModelId, ModelObservationSource};

        use crate::event_log::{emit_one, resolve_events_log_path};
        use crate::events::worker_spawn::emit_new_model_observations;

        let dir = tempdir().unwrap();
        let m = mol("task-20260729-7dd4");
        let log = resolve_events_log_path(dir.path());
        let worker = WorkerId::new("worker-selfhosted").unwrap();

        emit_one(
            &log,
            EventV2::AdapterSelected {
                mol_id: m.clone(),
                adapter_name: "local".to_owned(),
                selected_at: Utc::now(),
                selection_source: AdapterSelectionSource::Cli {
                    flag: "local".to_owned(),
                },
                role_hint: None,
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();
        emit_one(
            &log,
            EventV2::WorkerSpawned {
                worker_id: worker.clone(),
                molecule: Some(m.clone()),
                session_name: "seat".to_owned(),
                role: "worker".to_owned(),
                adapter_name: "local".to_owned(),
                loop_ownership: LoopOwnershipTag::default(),
            },
            None,
        )
        .unwrap();

        // The self-hosted case: everything about the method is pinnable.
        let pinned = AlgorithmicProvenance::hosted_unverifiable("ollama@localhost")
            .with_weights(WeightsProvenance::Pinned {
                algorithm: "sha256".to_owned(),
                digest: "cd".repeat(32),
            })
            .with_quantization(Disclosure::Observed("q4_K_M".to_owned()))
            .with_decoding(DecodingParams {
                temperature: Disclosure::Observed(0.0),
                top_p: Disclosure::Observed(1.0),
                seed: Disclosure::Observed(7),
            })
            .with_prompt_context(b"the briefing bytes");
        emit_new_model_observations(
            dir.path(),
            &m,
            &worker,
            "local",
            &[ModelId::new("gpt-oss:120b").unwrap()],
            ModelObservationSource::ProviderResponse,
            &pinned,
        );

        let read = realized_provenance(dir.path(), &m).expect("provenance present");
        assert_eq!(read, pinned, "the record survives serde byte for byte");
        assert!(
            read.is_algorithm_replayable(),
            "a self-hosted model pins every leg of the method"
        );
    }

    /// The provenance is scoped to the attempt exactly like the realized id: a
    /// re-tackle under a new worker must not inherit the previous attempt's
    /// weights digest. Otherwise the record would attest to a method that did
    /// not produce this conclusion — the failure mode the whole primitive
    /// exists to prevent.
    #[test]
    fn a_retackle_does_not_inherit_the_previous_attempts_provenance() {
        use cosmon_core::algorithmic_provenance::{AlgorithmicProvenance, WeightsProvenance};
        use cosmon_core::event_v2::{AdapterSelectionSource, LoopOwnershipTag};
        use cosmon_core::id::WorkerId;
        use cosmon_core::model_realization::{ModelId, ModelObservationSource};

        use crate::event_log::{emit_one, resolve_events_log_path};
        use crate::events::worker_spawn::emit_new_model_observations;

        let dir = tempdir().unwrap();
        let m = mol("task-20260729-9c11");
        let log = resolve_events_log_path(dir.path());

        let spawn = |worker: &WorkerId| {
            emit_one(
                &log,
                EventV2::AdapterSelected {
                    mol_id: m.clone(),
                    adapter_name: "local".to_owned(),
                    selected_at: Utc::now(),
                    selection_source: AdapterSelectionSource::Cli {
                        flag: "local".to_owned(),
                    },
                    role_hint: None,
                    loop_ownership: LoopOwnershipTag::default(),
                },
                None,
            )
            .unwrap();
            emit_one(
                &log,
                EventV2::WorkerSpawned {
                    worker_id: worker.clone(),
                    molecule: Some(m.clone()),
                    session_name: "seat".to_owned(),
                    role: "worker".to_owned(),
                    adapter_name: "local".to_owned(),
                    loop_ownership: LoopOwnershipTag::default(),
                },
                None,
            )
            .unwrap();
        };

        // Attempt 1 — weights pinned.
        let first = WorkerId::new("worker-attempt-1").unwrap();
        spawn(&first);
        emit_new_model_observations(
            dir.path(),
            &m,
            &first,
            "local",
            &[ModelId::new("gpt-oss:120b").unwrap()],
            ModelObservationSource::ProviderResponse,
            &AlgorithmicProvenance::hosted_unverifiable("ollama@localhost").with_weights(
                WeightsProvenance::Pinned {
                    algorithm: "sha256".to_owned(),
                    digest: "ef".repeat(32),
                },
            ),
        );

        // Attempt 2 — a hosted frontier model, which can pin nothing.
        let second = WorkerId::new("worker-attempt-2").unwrap();
        spawn(&second);
        emit_new_model_observations(
            dir.path(),
            &m,
            &second,
            "local",
            &[ModelId::new("gpt-5.6-terra").unwrap()],
            ModelObservationSource::ProviderResponse,
            &AlgorithmicProvenance::hosted_unverifiable("api.example.com"),
        );

        let read = realized_provenance(dir.path(), &m).expect("provenance present");
        assert!(
            !read.weights.is_verifiable(),
            "the second attempt's hosted weights must not be reported as pinned \
             by the first attempt's digest"
        );
        assert!(!read.is_algorithm_replayable());
    }

    /// A molecule with nothing in the log projects to `None` — an advisory
    /// read, never a fabricated attribution.
    #[test]
    fn realized_attribution_absent_for_an_unknown_molecule() {
        let dir = tempdir().unwrap();
        assert!(realized_attribution(dir.path(), &mol("task-20260728-none")).is_none());
    }
}
