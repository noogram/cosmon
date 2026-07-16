// SPDX-License-Identifier: AGPL-3.0-only

//! **Live** mode-C survival check against a pinned ollama endpoint
//! (delib-20260707-df9b M2 validation, D-A).
//!
//! This is the honest counterpart to the deterministic mock bench
//! (`mode_c_falsification_bench.rs`): it drives cosmon's **real**
//! [`run_agent_loop`] — compiled from *this* worktree, so it exercises the
//! streaming own-side extraction under test — against the live `gpt-oss:120b`
//! model on the pinned ollama build whose server-side tool-call parser is what
//! returned HTTP 500 under `stream:false` (task-20260707-c253).
//!
//! It is `#[ignore]` by default (needs the model + endpoint present) and pinned
//! via env, matching `scripts/mode-c-bench/lib.sh`:
//!
//! ```sh
//! BENCH_OLLAMA=http://127.0.0.1:11436 BENCH_MODEL=gpt-oss:120b \
//!   cargo test -p cosmon-provider --test openai_g5_live_mode_c -- --ignored --nocapture
//! ```
//!
//! Verdict: **RECOVERED** iff `run_agent_loop` returns `Ok` (the worker
//! survived — no `stream:false` HTTP 500 killed it) with a non-empty synthesis
//! and/or a written artefact. A `ToolCallParse` / `ServerError` death would be
//! the mode-C failure this change exists to prevent.

#![cfg(feature = "http")]

use std::time::Duration;

use cosmon_provider::openai::{run_agent_loop, OpenAIProvider};

/// The pinned falsification provocation — kept for the survival probe. gpt-oss
/// routes its whole prose answer to the OpenAI `reasoning` channel on this
/// pure-text mission and may emit neither `content` nor a tool call, so it
/// proves *survival* (no HTTP 500 death) but not artefact production.
const SURVIVAL_MISSION: &str =
    include_str!("../../../scripts/mode-c-bench/provocation/anharmonic-mission.md");

/// A **tool-forcing** mission: the model must call `write_file` to satisfy it.
/// This is the deterministic live proof that a *streamed* tool call is
/// extracted own-side, dispatched, and lands an artefact on disk — the mode-C
/// mechanism M2 delivers.
const TOOL_FORCING_MISSION: &str = "Create a file named `mode_c_ok.txt` in the current \
     directory whose exact contents are the single line `own-side extraction works`. \
     Use the write_file tool to do it, then reply with a one-line confirmation and stop.";

fn endpoint() -> String {
    std::env::var("BENCH_OLLAMA").unwrap_or_else(|_| "http://127.0.0.1:11436".to_owned())
}

fn model() -> String {
    std::env::var("BENCH_MODEL").unwrap_or_else(|_| "gpt-oss:120b".to_owned())
}

fn live_provider() -> OpenAIProvider {
    // ollama ignores the bearer token; any non-empty value is fine.
    OpenAIProvider::with_base_url("ollama", model(), endpoint())
        // A generous per-request timeout: a 120B model streaming an 8-step
        // derivation can take a while per turn.
        .with_timeout(Duration::from_secs(600))
}

/// **RECOVERED** — a *streamed* tool call is extracted own-side, dispatched,
/// and lands an artefact on disk against the live pinned model. This is the
/// deterministic proof of the M2 mechanism: with `stream:true` ollama performs
/// no server-side tool-call parse (the HTTP 500 trigger), cosmon accumulates
/// the streamed `arguments` itself, and the reconstructed call runs.
#[tokio::test]
#[ignore = "live: requires pinned ollama endpoint + gpt-oss:120b (BENCH_OLLAMA / BENCH_MODEL)"]
async fn streamed_tool_call_lands_artefact_against_g5() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = live_provider();

    let synthesis = run_agent_loop(&provider, TOOL_FORCING_MISSION, dir.path(), None)
        .await
        .expect("mode-C worker must SURVIVE and dispatch the streamed tool call (no ollama 500)");

    let artefacts: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read work_dir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    eprintln!(
        "RECOVERED — streamed tool call dispatched own-side.\n  synthesis: {synthesis:?}\n  artefacts: {artefacts:?}"
    );
    assert!(
        artefacts.iter().any(|f| f == "mode_c_ok.txt"),
        "the streamed write_file call must have landed its artefact; got {artefacts:?}"
    );
}

/// **SURVIVAL** — the real mode-C worker path on the pinned falsification
/// provocation must not die. With `stream:true` the whole-script tool call that
/// fired the HTTP 500 under `stream:false` now streams back as raw `arguments`
/// cosmon accumulates itself, so `run_agent_loop` returns `Ok` rather than a
/// `ToolCallParse` / `ServerError` death — even when gpt-oss emits its prose on
/// the `reasoning` channel and leaves the final `content` empty.
#[tokio::test]
#[ignore = "live: requires pinned ollama endpoint + gpt-oss:120b (BENCH_OLLAMA / BENCH_MODEL)"]
async fn anharmonic_provocation_survives_mode_c_against_g5() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = live_provider();

    let synthesis = run_agent_loop(&provider, SURVIVAL_MISSION, dir.path(), None)
        .await
        .expect("mode-C worker must SURVIVE the live provocation (stream:true suppresses the 500)");
    eprintln!("SURVIVED — no HTTP-500 death. final synthesis: {synthesis:?}");
}
