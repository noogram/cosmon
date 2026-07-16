// SPDX-License-Identifier: AGPL-3.0-only

//! `observe` operation tool — read-only single-molecule state projection.
//!
//! Wraps the [`cosmon_state::ops::observe`] verb as a
//! [`cosmon_agent_harness::Tool`]. The model emits
//! `{"molecule_id": "task-…"}`; the tool loads the project's
//! [`FileStore`] in-process and returns the byte-stable
//! [`cosmon_state::ops::ObserveJson`] projection — the *same* JSON shape
//! `cs observe <id> --json` prints, so a model that learned the `cs`
//! surface reads the tool output unchanged.
//!
//! No subprocess: the verb is called directly against `cosmon-state`
//! (delib §4 — efficiency via the internal API). JSON-in / JSON-out per
//! the claw-code tool contract borrowed as bibliography under ADR-096
//! §2.6; the name is the cosmon glossary verb `observe`, never a claw
//! `Plugin`.

use std::path::Path;

use cosmon_agent_harness::{ParametersSchema, Tool, ToolDeclaration, ToolError};
use cosmon_core::auth::Subject;
use cosmon_core::id::MoleculeId;
use cosmon_state::ops::{observe, ObserveJson};
use serde::Deserialize;

use crate::{io_err, parse_args, resolve_store};

/// Arguments for the `observe` tool — a single molecule id.
///
/// Single-field shape mirrors the `cs observe <id>` CLI surface; no
/// `--probe` / `--energy` flags in v0 (the verb already folds the
/// coupling report + ghost detection into [`ObserveJson`]).
#[derive(Debug, Deserialize)]
pub struct ObserveInput {
    /// The molecule id to inspect, e.g. `"task-20260531-ffed"`.
    pub molecule_id: String,
}

/// `observe` — read-only inspection of one molecule's lifecycle state.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObserveTool;

impl Tool for ObserveTool {
    fn name(&self) -> &'static str {
        "observe"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration::new(
            "observe",
            "Inspect one cosmon molecule's lifecycle state by id. Returns the \
             molecule's status, current/total steps, completed steps, assigned \
             worker, typed DAG links, tags, timestamps, coupling report and any \
             ghost marker — the same JSON `cs observe <id> --json` prints. \
             Read-only.",
            ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "molecule_id": {
                        "type": "string",
                        "description": "Molecule id to inspect, e.g. 'task-20260531-ffed'."
                    }
                },
                "required": ["molecule_id"],
            })),
        )
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let input: ObserveInput = parse_args("observe", arguments_json)?;
        let id = MoleculeId::new(&input.molecule_id).map_err(|e| ToolError::InvalidArguments {
            tool: "observe".to_owned(),
            message: e.to_string(),
        })?;

        let (store, state_dir) = resolve_store(work_dir);
        // Read-only verb; `Subject::operator()` is the trusted in-process
        // CLI subject (the tool runs avatar-side, same trust as `cs`).
        let view = observe(&store, &state_dir, &Subject::operator(), &id).map_err(io_err)?;

        // The molecule directory is not exposed through the StateStore
        // trait; the file-backed adapter computes it. cs-cli threads the
        // same value into the wire shape.
        let molecule_dir = store.molecule_dir(&id).to_string_lossy().into_owned();
        let json = ObserveJson::from_view(&view, &molecule_dir);
        serde_json::to_string(&json).map_err(io_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::seed_molecule;

    #[test]
    fn declaration_names_the_tool_and_requires_molecule_id() {
        let decl = ObserveTool.declaration();
        assert_eq!(decl.name, "observe");
        let schema = decl.parameters.as_json();
        assert_eq!(schema["required"][0], "molecule_id");
    }

    #[test]
    fn invalid_json_is_invalid_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let err = ObserveTool
            .execute("not json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn observe_known_molecule_returns_state_json() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");

        let args = serde_json::json!({ "molecule_id": "task-20260531-aaaa" });
        let raw = ObserveTool
            .execute(&args.to_string(), fixture.path())
            .expect("observe must succeed");

        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["id"], "task-20260531-aaaa");
        assert_eq!(parsed["status"], "running");
        // Coupling-report scalars present (single-snapshot invariants).
        assert_eq!(parsed["poll_count"], 1);
    }

    #[test]
    fn observe_unknown_molecule_is_io_error() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");

        let args = serde_json::json!({ "molecule_id": "task-20260531-zzzz" });
        let err = ObserveTool
            .execute(&args.to_string(), fixture.path())
            .expect_err("unknown molecule must fail");
        assert!(matches!(err, ToolError::Io(_)));
        assert!(err.to_string().contains("not found"));
    }
}
