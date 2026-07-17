// SPDX-License-Identifier: AGPL-3.0-only

//! `ensemble` operation tool — read-only filtered backlog snapshot.
//!
//! Wraps the [`cosmon_state::ops::ensemble`](fn@cosmon_state::ops::ensemble) verb as a
//! [`cosmon_agent_harness::Tool`]. The model emits an optional filter
//! (`status` / `kind` / `tags` / `fleet`); the tool returns the
//! byte-stable [`cosmon_state::ops::EnsembleJson`] index — the slim
//! per-molecule shape `cs ensemble` prints, one entry per match.
//!
//! This is the tool a pilot reaches for to answer "what is on the
//! `temp:hot` shelf right now": `{"tags": ["temp:hot"]}`. The verb is
//! called directly against `cosmon-state` (delib `2026-05-31` §4 —
//! efficiency via the internal API), never via a `cs ensemble`
//! subprocess.
//!
//! JSON-in / JSON-out per the claw-code tool contract borrowed as
//! bibliography under ADR-096 §2.6; `ensemble` is a cosmon glossary verb,
//! not a claw `Plugin`.

use std::path::Path;

use cosmon_agent_harness::{ParametersSchema, Tool, ToolDeclaration, ToolError};
use cosmon_core::auth::Subject;
use cosmon_state::ops::{ensemble, EnsembleError, EnsembleJson, EnsembleRequest};
use serde::Deserialize;

use crate::{io_err, parse_args, resolve_store};

/// Arguments for the `ensemble` tool — every filter optional.
///
/// Mirrors `cs ensemble`'s filter surface. An empty object lists every
/// molecule; each field narrows the slice. `tags` maps to the verb's
/// repeated `?tag=` query parameter — each entry is a glob matched
/// against the molecule's tag set (e.g. `"temp:hot"`, `"deferred:*"`).
#[derive(Debug, Default, Deserialize)]
pub struct EnsembleInput {
    /// Filter by lifecycle status (`running`, `pending`, …). Omit for all.
    #[serde(default)]
    pub status: Option<String>,
    /// Filter by molecule kind (`task`, `idea`, `decision`, …). Omit for all.
    #[serde(default)]
    pub kind: Option<String>,
    /// Tag glob filters; a molecule matches if any of its tags matches any
    /// glob. Empty → no tag filter.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Filter by fleet id. Omit to read the whole state store.
    #[serde(default)]
    pub fleet: Option<String>,
}

/// `ensemble` — read-only filtered listing of molecules.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnsembleTool;

impl Tool for EnsembleTool {
    fn name(&self) -> &'static str {
        "ensemble"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration::new(
            "ensemble",
            "List cosmon molecules matching an optional filter (status, kind, tags, \
             fleet). Returns a slim index — id, formula, status, steps, worker, tags \
             — one entry per match, the same shape `cs ensemble` prints. Use \
             tags=['temp:hot'] for the actionable backlog. Read-only; reach for \
             `observe <id>` to expand any single entry.",
            ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "description": "Filter by lifecycle status, e.g. 'running', 'pending'."
                    },
                    "kind": {
                        "type": "string",
                        "description": "Filter by molecule kind, e.g. 'task', 'idea', 'decision'."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tag glob filters, e.g. ['temp:hot']. \
                            A molecule matches if any tag matches any glob."
                    },
                    "fleet": {
                        "type": "string",
                        "description": "Filter by fleet id. Omit to read the whole store."
                    }
                },
            })),
        )
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let input: EnsembleInput = parse_args("ensemble", arguments_json)?;
        let (store, state_dir) = resolve_store(work_dir);

        let request = EnsembleRequest {
            status: input.status,
            kind: input.kind,
            tag_globs: input.tags,
            fleet: input.fleet,
        };

        let view = ensemble(&store, &state_dir, &Subject::operator(), request)
            .map_err(map_ensemble_err)?;
        let json = EnsembleJson::from_view(&view);
        serde_json::to_string(&json).map_err(io_err)
    }
}

/// Map an [`EnsembleError`] onto the harness's [`ToolError`].
///
/// A bad filter value (`status=foo`) is the model's mistake, so it maps to
/// [`ToolError::InvalidArguments`] — the model should retry with a valid
/// value. A store read failure maps to [`ToolError::Io`].
fn map_ensemble_err(err: EnsembleError) -> ToolError {
    match err {
        EnsembleError::InvalidFilter(message) => ToolError::InvalidArguments {
            tool: "ensemble".to_owned(),
            message,
        },
        EnsembleError::StoreUnavailable(_) => io_err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::{seed_molecule, seed_molecule_tagged};

    #[test]
    fn declaration_names_the_tool() {
        assert_eq!(EnsembleTool.name(), "ensemble");
        assert_eq!(EnsembleTool.declaration().name, "ensemble");
    }

    #[test]
    fn invalid_json_is_invalid_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let err = EnsembleTool
            .execute("nope", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn empty_filter_lists_all_molecules() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");
        seed_molecule(fixture.path(), "task-20260531-bbbb", "pending");

        let raw = EnsembleTool
            .execute("{}", fixture.path())
            .expect("ensemble must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["total"], 2);
    }

    #[test]
    fn status_filter_narrows_the_slice() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");
        seed_molecule(fixture.path(), "task-20260531-bbbb", "pending");

        let raw = EnsembleTool
            .execute(
                &serde_json::json!({ "status": "running" }).to_string(),
                fixture.path(),
            )
            .expect("ensemble must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["molecules"][0]["status"], "running");
    }

    #[test]
    fn tag_glob_filter_selects_temp_hot() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule_tagged(
            fixture.path(),
            "task-20260531-aaaa",
            "running",
            &["temp:hot"],
        );
        seed_molecule_tagged(
            fixture.path(),
            "task-20260531-bbbb",
            "running",
            &["temp:warm"],
        );

        let raw = EnsembleTool
            .execute(
                &serde_json::json!({ "tags": ["temp:hot"] }).to_string(),
                fixture.path(),
            )
            .expect("ensemble must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["total"], 1);
        assert_eq!(parsed["molecules"][0]["id"], "task-20260531-aaaa");
    }

    #[test]
    fn garbage_status_is_invalid_arguments() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");

        let err = EnsembleTool
            .execute(
                &serde_json::json!({ "status": "not-a-status" }).to_string(),
                fixture.path(),
            )
            .expect_err("garbage status must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
