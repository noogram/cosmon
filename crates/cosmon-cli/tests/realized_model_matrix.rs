// SPDX-License-Identifier: AGPL-3.0-only

//! F-06 — end-to-end **integration matrix** for realized-model capture
//! (delib-20260718-c70e / task-20260718-a550).
//!
//! The pre-mortem's charge was that the green unit tests proved isolated
//! functions, not the contract: fixture bytes → parse → `ModelObserved` on
//! `events.jsonl` → retrospective fold of the **last attempt** →
//! `compact_cell`. This file walks that whole pipeline through the real event
//! log (`emit_new_model_observations` → `read_all` → `AdapterAttribution::fold`)
//! for every adapter family and every failure mode the audit named:
//!
//! - per adapter: claude (session jsonl), codex (turn_context), provider
//!   (openai/anthropic/mistral response body);
//! - per failure mode: re-tackle same adapter, adapter change, late observation
//!   from a dead worker, worker dead before any event, capable adapter that
//!   completed unobserved, and a structurally-mute adapter's silence.

use cosmon_core::adapter_attribution::{AdapterAttribution, Realized};
use cosmon_core::event_v2::{
    AdapterSelectionSource, EventV2, ModelSelectionSource,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::model_realization::{
    realized_model_from_provider_response, realized_models_from_claude_jsonl,
    realized_models_from_codex_session, ModelId, ModelObservationSource,
};
use cosmon_state::event_log::{emit_one, read_all, resolve_events_log_path};
use cosmon_state::events::worker_spawn::emit_new_model_observations;

fn mol(s: &str) -> MoleculeId {
    MoleculeId::new(s).unwrap()
}

fn seed_adapter(dir: &std::path::Path, m: &MoleculeId, adapter: &str) {
    let log = resolve_events_log_path(dir);
    emit_one(
        &log,
        EventV2::AdapterSelected {
            mol_id: m.clone(),
            adapter_name: adapter.to_owned(),
            selected_at: chrono::Utc::now(),
            selection_source: AdapterSelectionSource::Cli {
                flag: adapter.to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )
    .unwrap();
}

fn seed_worker(dir: &std::path::Path, m: &MoleculeId, adapter: &str, worker: &str) {
    let log = resolve_events_log_path(dir);
    emit_one(
        &log,
        EventV2::WorkerSpawned {
            worker_id: WorkerId::new(worker).unwrap(),
            molecule: Some(m.clone()),
            session_name: "sess".to_owned(),
            role: "polecat".to_owned(),
            adapter_name: adapter.to_owned(),
            loop_ownership: Default::default(),
        },
        None,
    )
    .unwrap();
}

fn seed_model(dir: &std::path::Path, m: &MoleculeId, adapter: &str, model: Option<&str>) {
    let log = resolve_events_log_path(dir);
    emit_one(
        &log,
        EventV2::ModelSelected {
            mol_id: m.clone(),
            adapter_name: adapter.to_owned(),
            model: model.map(ToOwned::to_owned),
            selection_source: match model {
                Some(f) => ModelSelectionSource::Flag { flag: f.to_owned() },
                None => ModelSelectionSource::Default {
                    fallback_reason: "floor".to_owned(),
                },
            },
            selected_at: chrono::Utc::now(),
        },
        None,
    )
    .unwrap();
}

fn seed_completed(dir: &std::path::Path, m: &MoleculeId) {
    let log = resolve_events_log_path(dir);
    emit_one(
        &log,
        EventV2::MoleculeCompleted {
            molecule_id: m.clone(),
            duration_ms: None,
            reason: "done".to_owned(),
        },
        None,
    )
    .unwrap();
}

fn seed_exited(dir: &std::path::Path, m: &MoleculeId) {
    let log = resolve_events_log_path(dir);
    emit_one(
        &log,
        EventV2::WorkerExited {
            molecule_id: m.clone(),
            exit_code: Some(137),
            reason: "pane_died".to_owned(),
        },
        None,
    )
    .unwrap();
}

fn fold(dir: &std::path::Path, m: &MoleculeId) -> AdapterAttribution {
    let events: Vec<EventV2> = read_all(resolve_events_log_path(dir))
        .unwrap()
        .into_iter()
        .filter(|e| e.event.molecule_id() == Some(m))
        .map(|e| e.event)
        .collect();
    AdapterAttribution::fold(&events)
}

fn ids(models: &[&str]) -> Vec<ModelId> {
    models.iter().map(|m| ModelId::new(m).unwrap()).collect()
}

// ---- per-adapter, full pipeline ------------------------------------------

#[test]
fn claude_fixture_to_compact_cell() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa01");
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-1");
    seed_model(dir.path(), &m, "claude", Some("claude-opus-4-8"));

    // Fixture: a real Opus→Sonnet quota fallback.
    let fixture = concat!(
        r#"{"type":"system","subtype":"init","model":"claude-opus-4-8"}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-opus-4-8"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-sonnet-5"}}"#,
    );
    let observed = realized_models_from_claude_jsonl(fixture);
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "claude",
        &observed,
        ModelObservationSource::ClaudeStreamJson,
    );

    let att = fold(dir.path(), &m);
    assert_eq!(
        att.realized,
        Realized::Observed(vec![
            "claude-opus-4-8".to_string(),
            "claude-sonnet-5".to_string(),
        ])
    );
    assert_eq!(
        att.compact_cell(),
        "claude/claude-opus-4-8~>claude-sonnet-5 [cli]"
    );
}

#[test]
fn codex_turn_context_fixture_to_compact_cell() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa02");
    seed_adapter(dir.path(), &m, "codex");
    seed_worker(dir.path(), &m, "codex", "worker-1");

    let fixture = concat!(
        r#"{"type":"session_meta","payload":{"cwd":"/w","session_id":"s"}}"#,
        "\n",
        r#"{"type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#,
    );
    let observed = realized_models_from_codex_session(fixture);
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "codex",
        &observed,
        ModelObservationSource::CodexSessionMeta,
    );

    let att = fold(dir.path(), &m);
    assert_eq!(
        att.realized,
        Realized::Observed(vec!["gpt-5.6-terra".to_string()])
    );
    // Unpinned dispatch that still ran a concrete model → `~>` without `/`.
    assert_eq!(att.compact_cell(), "codex~>gpt-5.6-terra [cli]");
}

#[test]
fn provider_response_fixture_to_compact_cell() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa03");
    seed_adapter(dir.path(), &m, "openai");
    seed_worker(dir.path(), &m, "openai", "worker-1");
    seed_model(dir.path(), &m, "openai", Some("gpt-5"));

    // The provider echoed a *different* served id than the pin → honest drift.
    let body = r#"{"id":"chatcmpl-1","model":"gpt-5-2026-07-01","choices":[]}"#;
    let observed: Vec<ModelId> = realized_model_from_provider_response(body)
        .into_iter()
        .collect();
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "openai",
        &observed,
        ModelObservationSource::ProviderResponse,
    );

    let att = fold(dir.path(), &m);
    assert_eq!(
        att.realized,
        Realized::Observed(vec!["gpt-5-2026-07-01".to_string()])
    );
    assert_eq!(att.compact_cell(), "openai/gpt-5~>gpt-5-2026-07-01 [cli]");
}

// ---- failure modes --------------------------------------------------------

#[test]
fn adapter_change_does_not_inherit_prior_realization() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa04");
    // Attempt 1: claude runs opus, then crashes.
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-1");
    seed_model(dir.path(), &m, "claude", Some("claude-opus-4-8"));
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "claude",
        &ids(&["claude-opus-4-8"]),
        ModelObservationSource::ClaudeStreamJson,
    );
    seed_exited(dir.path(), &m);
    // Attempt 2: re-tackle to codex, no observation.
    seed_adapter(dir.path(), &m, "codex");
    seed_worker(dir.path(), &m, "codex", "worker-2");
    seed_model(dir.path(), &m, "codex", Some("gpt-5-codex"));

    let att = fold(dir.path(), &m);
    // The codex attempt is honestly Unknown — it never inherits opus.
    assert_eq!(att.realized, Realized::Unknown);
    assert_eq!(att.compact_cell(), "codex/gpt-5-codex [cli] ?");
}

#[test]
fn re_tackle_same_adapter_shows_only_last_attempt() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa05");
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-1");
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "claude",
        &ids(&["claude-opus-4-8"]),
        ModelObservationSource::ClaudeStreamJson,
    );
    seed_exited(dir.path(), &m);
    // Re-tackle same adapter, fresh worker observes sonnet.
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-2");
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-2").unwrap()),
        "claude",
        &ids(&["claude-sonnet-5"]),
        ModelObservationSource::ClaudeStreamJson,
    );

    let att = fold(dir.path(), &m);
    assert_eq!(
        att.realized,
        Realized::Observed(vec!["claude-sonnet-5".to_string()])
    );
}

#[test]
fn late_observation_from_dead_worker_is_ignored() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa06");
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-2");
    // A straggler write attributed to the OLD worker-1 lands after worker-2's
    // spawn — the scope guard must drop it.
    emit_new_model_observations(
        dir.path(),
        &m,
        Some(&WorkerId::new("worker-1").unwrap()),
        "claude",
        &ids(&["claude-opus-4-8"]),
        ModelObservationSource::ClaudeStreamJson,
    );

    let att = fold(dir.path(), &m);
    assert_eq!(att.realized, Realized::Unknown);
}

#[test]
fn worker_dead_before_any_observation_is_unknown() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa07");
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-1");
    seed_model(dir.path(), &m, "claude", Some("claude-opus-4-8"));
    seed_exited(dir.path(), &m);

    let att = fold(dir.path(), &m);
    assert_eq!(att.realized, Realized::Unknown);
    assert_eq!(att.compact_cell(), "claude/claude-opus-4-8 [cli] ?");
}

#[test]
fn capable_adapter_completed_unobserved_is_unknown() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa08");
    // No observer ever ran; the molecule completed. A capable adapter's
    // completion is NOT silence — it is Unknown (F-05).
    seed_adapter(dir.path(), &m, "claude");
    seed_worker(dir.path(), &m, "claude", "worker-1");
    seed_model(dir.path(), &m, "claude", Some("claude-opus-4-8"));
    seed_completed(dir.path(), &m);

    let att = fold(dir.path(), &m);
    assert_eq!(att.realized, Realized::Unknown);
    assert_eq!(att.compact_cell(), "claude/claude-opus-4-8 [cli] ?");
}

#[test]
fn mute_adapter_completion_is_silent() {
    let dir = tempfile::TempDir::new().unwrap();
    let m = mol("task-20260718-aa09");
    // aider cannot structurally report a model → its completion is honest
    // silence, rendered `-`.
    seed_adapter(dir.path(), &m, "aider");
    seed_worker(dir.path(), &m, "aider", "worker-1");
    seed_model(dir.path(), &m, "aider", Some("gpt-4o"));
    seed_completed(dir.path(), &m);

    let att = fold(dir.path(), &m);
    assert_eq!(att.realized, Realized::Silent);
    assert_eq!(att.compact_cell(), "aider/gpt-4o [cli] -");
}
