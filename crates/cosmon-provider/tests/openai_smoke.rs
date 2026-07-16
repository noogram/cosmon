// SPDX-License-Identifier: AGPL-3.0-only

//! Smoke test for the OpenAI Direct-API adapter (ADR-100 R2 wave 2).
//!
//! `#[ignore]` by default — the test only meaningfully exercises the loop
//! against a live OpenAI endpoint, and only when `OPENAI_API_KEY` is set
//! AND the operator opts in via `OPENAI_LIVE_SMOKE=1`. Two-gate design so
//! a stray `OPENAI_API_KEY` in the operator's shell does not silently
//! consume budget on every `cargo test`.
//!
//! To run the live smoke locally:
//!
//! ```bash
//! OPENAI_LIVE_SMOKE=1 OPENAI_API_KEY=sk-… \
//!   cargo test --package cosmon-provider --test openai_smoke -- --ignored
//! ```
//!
//! The test asserts: (a) the agent loop returns successfully, (b) the
//! `haiku.md` artifact lands in the work_dir, (c) the `events.jsonl`
//! emits at least one `WorkerSpawnAttempted` with `adapter_name="openai"`.

#![cfg(feature = "http")]

use std::path::PathBuf;

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_provider::openai::{run_agent_loop, telemetry_for, OpenAIProvider};

#[tokio::test]
#[ignore]
async fn openai_haiku_smoke() {
    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("OPENAI_API_KEY unset — skipping live smoke");
            return;
        }
    };
    if std::env::var("OPENAI_LIVE_SMOKE").ok().as_deref() != Some("1") {
        eprintln!("OPENAI_LIVE_SMOKE != 1 — skipping live smoke");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let work_dir: PathBuf = dir.path().to_owned();
    let state_dir: PathBuf = dir.path().to_owned();

    let mol_id = MoleculeId::new("task-20260518-02bd").expect("mol id");
    let worker_id = WorkerId::new("openai-smoke-test").expect("worker id");
    let telemetry = telemetry_for(
        mol_id.clone(),
        worker_id,
        state_dir.clone(),
        "uuid-smoke-test",
    );

    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let provider = OpenAIProvider::new(api_key, model);

    let briefing = "Please write a short haiku about typed state machines and \
                    save it to `haiku.md` using the edit_file tool (empty search = \
                    create file). Then reply with a single-line synthesis and stop.";

    let synthesis = run_agent_loop(&provider, briefing, &work_dir, Some(&telemetry))
        .await
        .expect("agent loop must succeed when OPENAI_API_KEY is set");

    let haiku = work_dir.join("haiku.md");
    assert!(
        haiku.exists(),
        "haiku.md must be written by the agent via the edit_file tool"
    );
    assert!(!synthesis.trim().is_empty(), "synthesis must be non-empty");

    // Walk events.jsonl and assert the WorkerSpawnAttempted carries
    // adapter_name = "openai" — the cat-test invariant.
    let events_path = state_dir.join("events.jsonl");
    let raw = std::fs::read_to_string(&events_path)
        .expect("events.jsonl must be written by emit_worker_spawn_attempted");
    let mut saw_openai_spawn = false;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope: serde_json::Value = serde_json::from_str(line).expect("envelope json");
        if envelope.get("type").and_then(|v| v.as_str()) == Some("worker_spawn_attempted") {
            let adapter = envelope
                .get("adapter_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if adapter == "openai" {
                saw_openai_spawn = true;
                break;
            }
        }
    }
    assert!(
        saw_openai_spawn,
        "events.jsonl must contain at least one WorkerSpawnAttempted with adapter_name=\"openai\""
    );

    // Silence unused-import warning when the assertion path is reached
    // — the EventV2 import is here for future stricter parsing.
    let _ = std::mem::size_of::<EventV2>();
}
