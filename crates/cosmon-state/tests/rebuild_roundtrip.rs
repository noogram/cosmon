// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the events.jsonl → state.json cache-rebuild path.
//!
//! These exercise the full write-then-delete-then-rebuild loop so a future
//! regression that drops a seal, mis-orders fields, or introduces non-
//! determinism surfaces immediately.

use std::path::Path;

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::event_log::EventLogWriter;
use cosmon_state::{rebuild_molecule_state, BriefingSeal, MoleculeData, RebuildOutcome};
use tempfile::tempdir;

fn mid(s: &str) -> MoleculeId {
    MoleculeId::new(s).unwrap()
}

fn emit_all(path: &Path, events: Vec<EventV2>) {
    let mut w = EventLogWriter::open(path).unwrap();
    for ev in events {
        w.emit(ev, None).unwrap();
    }
    w.sync().unwrap();
}

/// Round-trip — nucleate + evolve events → delete state.json → rebuild →
/// the rebuilt cache must agree with the log on every field the log can know.
#[test]
fn roundtrip_delete_and_rebuild_preserves_status_and_seals() {
    let dir = tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mol_dir = dir
        .path()
        .join("fleets/default/molecules/task-20260420-rtt0");
    std::fs::create_dir_all(&mol_dir).unwrap();
    let state_path = mol_dir.join("state.json");
    let id = mid("task-20260420-rtt0");

    // Emit a full nucleate → evolve → complete sequence with seals.
    let now = chrono::Utc::now();
    emit_all(
        &events_path,
        vec![
            EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            },
            EventV2::PromptSealed {
                molecule_id: id.clone(),
                hash: "aaaa".repeat(16),
                sealed_at: now,
                bytes: 100,
                canonical_version: 1,
            },
            EventV2::MoleculeStatusChanged {
                molecule_id: id.clone(),
                from: "pending".into(),
                to: "running".into(),
            },
            EventV2::MoleculeStepCompleted {
                molecule_id: id.clone(),
                step: 0,
                total: 2,
                duration_ms: Some(1234),
                step_hash: None,
            },
            EventV2::BriefingSealed {
                molecule_id: id.clone(),
                step: 1,
                hash: "bbbb".repeat(16),
                sealed_at: now,
                bytes: 200,
                canonical_version: 1,
            },
            EventV2::MoleculeStepCompleted {
                molecule_id: id.clone(),
                step: 1,
                total: 2,
                duration_ms: Some(2345),
                step_hash: None,
            },
            EventV2::MoleculeCompleted {
                molecule_id: id.clone(),
                duration_ms: Some(5000),
                reason: "ok".into(),
            },
        ],
    );

    // First rebuild: cache is missing, create from events.
    let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
    assert_eq!(outcome, RebuildOutcome::CreatedFromEvents);

    let before: MoleculeData =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(before.status, MoleculeStatus::Completed);
    assert_eq!(before.total_steps, 2);
    assert_eq!(before.current_step, 2);
    assert_eq!(before.completed_steps.len(), 2);
    assert!(before.prompt_seal.is_some());
    assert_eq!(before.briefing_seals.len(), 1);

    // Delete the cache and rebuild again — the second rebuild must recover
    // the same state from the same events.
    std::fs::remove_file(&state_path).unwrap();
    let outcome2 = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
    assert_eq!(outcome2, RebuildOutcome::CreatedFromEvents);
    let after: MoleculeData = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();

    // Every event-derived field must match.
    assert_eq!(after.status, before.status);
    assert_eq!(after.total_steps, before.total_steps);
    assert_eq!(after.current_step, before.current_step);
    assert_eq!(after.completed_steps, before.completed_steps);
    assert_eq!(after.prompt_seal, before.prompt_seal);
    assert_eq!(after.briefing_seals, before.briefing_seals);
    assert_eq!(after.formula_id, before.formula_id);
}

/// Determinism — two independent rebuilds of the same events file produce
/// byte-identical state.json.
#[test]
fn two_rebuilds_produce_identical_bytes() {
    let dir = tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let id = mid("task-20260420-det0");

    let now = chrono::Utc::now();
    emit_all(
        &events_path,
        vec![
            EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            },
            EventV2::PromptSealed {
                molecule_id: id.clone(),
                hash: "c".repeat(64),
                sealed_at: now,
                bytes: 10,
                canonical_version: 1,
            },
            EventV2::MoleculeStepCompleted {
                molecule_id: id.clone(),
                step: 0,
                total: 1,
                duration_ms: None,
                step_hash: None,
            },
            EventV2::MoleculeCompleted {
                molecule_id: id.clone(),
                duration_ms: None,
                reason: "ok".into(),
            },
        ],
    );

    let dir_a = dir.path().join("a");
    std::fs::create_dir_all(&dir_a).unwrap();
    let path_a = dir_a.join("state.json");
    rebuild_molecule_state(&events_path, &id, &path_a).unwrap();

    let dir_b = dir.path().join("b");
    std::fs::create_dir_all(&dir_b).unwrap();
    let path_b = dir_b.join("state.json");
    rebuild_molecule_state(&events_path, &id, &path_b).unwrap();

    let bytes_a = std::fs::read(&path_a).unwrap();
    let bytes_b = std::fs::read(&path_b).unwrap();
    assert_eq!(bytes_a, bytes_b, "rebuilds must be byte-deterministic");
}

/// Corrupt cache → archived as .broken, then rebuilt fresh.
#[test]
fn corrupt_cache_archived_and_replaced() {
    let dir = tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mol_dir = dir
        .path()
        .join("fleets/default/molecules/task-20260420-cor0");
    std::fs::create_dir_all(&mol_dir).unwrap();
    let state_path = mol_dir.join("state.json");
    let id = mid("task-20260420-cor0");

    emit_all(
        &events_path,
        vec![EventV2::MoleculeNucleated {
            molecule_id: id.clone(),
            formula_id: "task-work".into(),
            parent_id: None,
            blocks: vec![],
        }],
    );
    std::fs::write(&state_path, b"{ this is not valid json").unwrap();

    let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
    assert_eq!(outcome, RebuildOutcome::RecoveredFromCorruption);

    let broken = state_path.with_extension("json.broken");
    assert!(broken.exists());
    let corrupted = std::fs::read(&broken).unwrap();
    assert_eq!(corrupted, b"{ this is not valid json");

    let data: MoleculeData = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(data.id, id);
    assert_eq!(data.formula_id.as_str(), "task-work");
}

/// Seals survive a rebuild — the prompt/briefing seal hashes read back from
/// `state.json` match what the event log recorded.
#[test]
fn seals_survive_rebuild() {
    let dir = tempdir().unwrap();
    let events_path = dir.path().join("events.jsonl");
    let mol_dir = dir
        .path()
        .join("fleets/default/molecules/task-20260420-sea0");
    std::fs::create_dir_all(&mol_dir).unwrap();
    let state_path = mol_dir.join("state.json");
    let id = mid("task-20260420-sea0");

    let now = chrono::Utc::now();
    let prompt_hash = "d".repeat(64);
    let step0_hash = "e".repeat(64);
    emit_all(
        &events_path,
        vec![
            EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            },
            EventV2::PromptSealed {
                molecule_id: id.clone(),
                hash: prompt_hash.clone(),
                sealed_at: now,
                bytes: 500,
                canonical_version: 1,
            },
            EventV2::BriefingSealed {
                molecule_id: id.clone(),
                step: 0,
                hash: step0_hash.clone(),
                sealed_at: now,
                bytes: 250,
                canonical_version: 1,
            },
        ],
    );

    rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
    let data: MoleculeData = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();

    let prompt = data.prompt_seal.expect("prompt seal");
    assert_eq!(prompt.hash, prompt_hash);
    assert_eq!(prompt.briefing_bytes, 500);

    assert_eq!(data.briefing_seals.len(), 1);
    let step0: &BriefingSeal = &data.briefing_seals[0];
    assert_eq!(step0.hash, step0_hash);
    assert_eq!(step0.step, 0);
    assert_eq!(step0.briefing_bytes, 250);
}
