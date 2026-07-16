// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end smoke test: drive the three read-only ops tools through the
//! shared [`cosmon_agent_harness::ToolRegistry`] dispatch path, exactly as
//! the cs-pilot loop will — `ToolCall` in, JSON string out — against a temp
//! `.cosmon/` project on disk.
//!
//! This is the integration-level mirror of the per-module unit tests: it
//! proves the registry builder wires every tool name to a working backend,
//! and that dispatch routes a model-shaped `ToolCall` to the right tool.

use cosmon_agent_harness::{ToolCall, ToolError};
use cosmon_filestore::FileStore;
use cosmon_ops_tools::read_only_registry;
use cosmon_state::StateStore;

mod fixture;

#[test]
fn registry_dispatches_observe_peek_ensemble_end_to_end() {
    let project = tempfile::tempdir().unwrap();
    fixture::seed_project(project.path());
    fixture::seed_molecule(project.path(), "task-20260531-aaaa", "running");
    fixture::seed_molecule(project.path(), "task-20260531-bbbb", "pending");

    let registry = read_only_registry();
    let work_dir = project.path();

    // peek — fleet/molecule overview.
    let peek = registry
        .execute(&ToolCall::new("c1", "peek", "{}"), work_dir)
        .expect("peek dispatch");
    let peek_json: serde_json::Value = serde_json::from_str(&peek).unwrap();
    assert_eq!(peek_json["total_molecules"], 2);

    // ensemble — filtered backlog snapshot.
    let ensemble = registry
        .execute(
            &ToolCall::new("c2", "ensemble", r#"{"status":"running"}"#),
            work_dir,
        )
        .expect("ensemble dispatch");
    let ensemble_json: serde_json::Value = serde_json::from_str(&ensemble).unwrap();
    assert_eq!(ensemble_json["total"], 1);

    // observe — single-molecule projection.
    let observe = registry
        .execute(
            &ToolCall::new("c3", "observe", r#"{"molecule_id":"task-20260531-aaaa"}"#),
            work_dir,
        )
        .expect("observe dispatch");
    let observe_json: serde_json::Value = serde_json::from_str(&observe).unwrap();
    assert_eq!(observe_json["id"], "task-20260531-aaaa");
    assert_eq!(observe_json["status"], "running");
}

#[test]
fn registry_refuses_a_write_verb() {
    // v0 is read-only: nucleate/tackle/done are NOT registered. A model
    // that hallucinates a write verb hits the registry's structural
    // refusal, never a silent no-op.
    let project = tempfile::tempdir().unwrap();
    fixture::seed_project(project.path());

    let registry = read_only_registry();
    let err = registry
        .execute(&ToolCall::new("c4", "nucleate", "{}"), project.path())
        .expect_err("nucleate must not be registered in v0");
    assert!(matches!(err, ToolError::NotWhitelisted(_)));
}

#[test]
fn observe_reports_directly_against_the_filestore() {
    // Sanity: the tool reads the same molecule a direct FileStore load
    // sees — proof the tool calls the internal API, not a `cs` subprocess.
    let project = tempfile::tempdir().unwrap();
    fixture::seed_molecule(project.path(), "task-20260531-cccc", "running");

    let store = FileStore::new(project.path().join(".cosmon").join("state"));
    let direct = store
        .load_molecule(&cosmon_core::id::MoleculeId::new("task-20260531-cccc").unwrap())
        .expect("direct load");
    assert_eq!(direct.id.as_str(), "task-20260531-cccc");

    let registry = read_only_registry();
    let raw = registry
        .execute(
            &ToolCall::new("c5", "observe", r#"{"molecule_id":"task-20260531-cccc"}"#),
            project.path(),
        )
        .expect("observe dispatch");
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["id"], direct.id.as_str());
}
