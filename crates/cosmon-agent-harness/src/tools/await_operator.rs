// SPDX-License-Identifier: AGPL-3.0-only

//! `await_operator` — the harness's **only** typed blocking primitive
//! (ADR-123).
//!
//! # Why this tool exists, and why it is gated
//!
//! The 2026-06-07/08 incident: a worker reached an
//! *undecidable-AND-irreversible* decision (rewrite a signable
//! instrument) and blocked through Claude Code's `AskUserQuestion` modal
//! — a surface **external** to cosmon's state machine. The molecule sat
//! `Running`, byte-indistinguishable from a healthy worker, and the DAG
//! drained nothing all night.
//!
//! The structural fix in the cosmon-native harness is twofold:
//!
//! 1. **No modal tool exists, ever.** The harness registry
//!    ([`crate::tool::default_registry`]) registers no `ask_user_question`
//!    / modal primitive by construction. A native worker therefore
//!    *cannot* raise an invisible block — it can only finish
//!    ([`crate::spine::Turn::Stop`]), keep working, or call this tool.
//! 2. **This tool is capability-gated.** It is registered only when the
//!    molecule carries an [`OperatorBlockCapability`] (the
//!    `op-block:<boundary>` tag granted at `cs nucleate`). A worker
//!    without the capability finds the tool *absent* — its only path is
//!    surface-and-continue. *Make blocking-without-emitting structurally
//!    hard, not merely documented* (kahneman).
//!
//! # What it does
//!
//! Shells the single emit path — `cs await-operator <id> --question …`
//! (CLI-first invariant: workers use the `cs` CLI, never an in-process
//! re-implementation). That verb writes `blocked_on.json`, emits
//! [`EventV2::WorkerBlockedOnOperator`], stamps `temp:awaiting-op`, and
//! routes on the capability. Emission lives in exactly one place; this
//! tool is the model-facing affordance over it.
//!
//! [`OperatorBlockCapability`]: cosmon_core::operator_block::OperatorBlockCapability
//! [`EventV2::WorkerBlockedOnOperator`]: cosmon_core::event_v2::EventV2

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::tool::{ParametersSchema, Tool, ToolDeclaration, ToolError};

/// Default `cs` binary name, resolved on `PATH` from the worker's
/// worktree (walk-up discovery — see CLAUDE.md "CLI over MCP for
/// workers").
const DEFAULT_CS_BIN: &str = "cs";

#[derive(Debug, Deserialize)]
struct AwaitParams {
    /// The molecule whose worker is blocking. The model knows this from
    /// its briefing.
    molecule_id: String,
    /// The decision(s) the operator is being asked to make.
    questions: Vec<String>,
}

/// `await_operator` — capability-gated typed blocking primitive.
///
/// Registered **only** for capability-bearing molecules (see
/// [`crate::tool::default_registry_with_operator_block`]). Carries the
/// `cs` binary name so tests can inject a stub; production uses
/// `DEFAULT_CS_BIN`.
#[derive(Debug, Clone)]
pub struct AwaitOperator {
    cs_bin: String,
}

impl Default for AwaitOperator {
    fn default() -> Self {
        Self {
            cs_bin: DEFAULT_CS_BIN.to_owned(),
        }
    }
}

impl AwaitOperator {
    /// Construct with a custom `cs` binary path (tests inject a stub).
    #[must_use]
    pub fn with_cs_bin(cs_bin: impl Into<String>) -> Self {
        Self {
            cs_bin: cs_bin.into(),
        }
    }
}

impl Tool for AwaitOperator {
    fn name(&self) -> &'static str {
        "await_operator"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration {
            name: "await_operator",
            description: "Pause for an operator decision at an IRREVERSIBLE boundary (a \
                signature about to be transmitted, a push to a shared remote, a publish, an \
                authoritative value downstream consumers act on). This is the ONLY sanctioned \
                way to block — it emits the typed cosmon signal and yields observably. Use it \
                ONLY when the next action is genuinely irreversible AND you cannot decide it \
                yourself; everything reversible (drafting, rewriting on the unmerged worktree) \
                must be done by picking a sensible default and continuing, never by blocking.",
            parameters: ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "molecule_id": {
                        "type": "string",
                        "description": "The molecule id from your briefing."
                    },
                    "questions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The decision(s) the operator must make before you act."
                    }
                },
                "required": ["molecule_id", "questions"]
            })),
        }
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let params: AwaitParams =
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                tool: "await_operator".to_owned(),
                message: e.to_string(),
            })?;
        if params.questions.iter().all(|q| q.trim().is_empty()) {
            return Err(ToolError::InvalidArguments {
                tool: "await_operator".to_owned(),
                message: "at least one non-empty question is required".to_owned(),
            });
        }

        // Single emit path: shell `cs await-operator` from the worktree
        // (walk-up discovery resolves the state store). CLI-first.
        let mut cmd = Command::new(&self.cs_bin);
        cmd.current_dir(work_dir)
            .arg("await-operator")
            .arg(&params.molecule_id);
        for q in &params.questions {
            cmd.arg("--question").arg(q);
        }
        let output = cmd.output().map_err(|e| {
            ToolError::Io(format!(
                "failed to run `{} await-operator`: {e}",
                self.cs_bin
            ))
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::Io(format!(
                "`{} await-operator` exited with {}: {stderr}",
                self.cs_bin, output.status
            )));
        }
        Ok(stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_names_the_tool_and_requires_molecule_and_questions() {
        let decl = AwaitOperator::default().declaration();
        assert_eq!(decl.name, "await_operator");
        let required = decl.parameters.as_json()["required"]
            .as_array()
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(names, vec!["molecule_id", "questions"]);
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = AwaitOperator::default()
            .execute("not-json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn empty_questions_are_rejected_before_shelling() {
        let dir = tempfile::tempdir().unwrap();
        let err = AwaitOperator::with_cs_bin("/nonexistent/cs")
            .execute(
                &serde_json::json!({ "molecule_id": "task-20260608-aaaa", "questions": ["  "] })
                    .to_string(),
                dir.path(),
            )
            .expect_err("must reject empty questions");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn shells_the_configured_cs_binary() {
        // Inject a stub `cs` that echoes a JSON line and exits 0, proving
        // the tool shells the configured binary with the molecule id.
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("cs-stub.sh");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho \"{\\\"status\\\":\\\"blocked\\\",\\\"argv\\\":\\\"$*\\\"}\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            let out = AwaitOperator::with_cs_bin(stub.to_string_lossy())
                .execute(
                    &serde_json::json!({
                        "molecule_id": "task-20260608-aaaa",
                        "questions": ["Sign the act?"]
                    })
                    .to_string(),
                    dir.path(),
                )
                .expect("stub must succeed");
            assert!(out.contains("await-operator"), "argv echoed: {out}");
            assert!(
                out.contains("task-20260608-aaaa"),
                "molecule id passed: {out}"
            );
        }
    }
}
