// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-pilot` binary — launch the interactive cognitive pilot against
//! a **local** Ollama model.
//!
//! This is the foreground entrypoint the (downstream) `cs pilot`
//! subcommand will shell into. It wires the default v0 surface:
//!
//! - provider: [`cosmon_provider::OpenAIProvider`] pointed at the local
//!   Ollama OpenAI-compatible endpoint (`http://localhost:11434/v1`), per
//!   the autonomy-local-first doctrine;
//! - tools: [`cosmon_ops_tools::read_only_registry`] (`observe` / `peek` /
//!   `ensemble`, read-only);
//! - transcript: `pilot-transcript.md` in the working directory.
//!
//! Knobs are environment variables so the binary needs no argument parser
//! in v0 (the `cs pilot` subcommand, a separate molecule, owns the flag
//! surface):
//!
//! - `COSMON_PILOT_MODEL`     — Ollama model tag (default `llama3.2`).
//! - `COSMON_PILOT_BASE_URL`  — override the endpoint (default Ollama).
//! - `COSMON_PILOT_TRANSCRIPT`— override the transcript path.

#![forbid(unsafe_code)]
// Proper nouns (OpenAI, Ollama) appear in the binary's module docs; same
// stance as the library crate root.
#![allow(clippy::doc_markdown)]

use std::io::{self, BufReader};
use std::path::PathBuf;

use cosmon_pilot::repl::{run_repl, ReplConfig};
use cosmon_pilot::transcript::Transcript;
use cosmon_provider::OpenAIProvider;

/// Default Ollama OpenAI-compatible endpoint. `OpenAIProvider` appends
/// `/v1/chat/completions`; the trailing `/v1` is normalised away by the
/// provider, so either form is accepted.
const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

/// Default local model tag — a small instruct model most Ollama installs
/// already have pulled. Override with `COSMON_PILOT_MODEL`.
const DEFAULT_MODEL: &str = "llama3.2";

/// The pilot persona / opening framing the session is seeded with.
const BRIEFING: &str = "You are the cosmon pilot — a cognitive co-pilot for an operator running \
     a fleet of AI coding agents. You can inspect the fleet with the read-only \
     tools `observe` (one molecule by id), `peek`, and `ensemble`. When the \
     operator asks about the state of a molecule or the backlog, call the \
     appropriate tool rather than guessing. Be concise.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let work_dir = std::env::current_dir()?;

    let base_url =
        std::env::var("COSMON_PILOT_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
    let model = std::env::var("COSMON_PILOT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let transcript_path = std::env::var("COSMON_PILOT_TRANSCRIPT")
        .map_or_else(|_| work_dir.join("pilot-transcript.md"), PathBuf::from);

    let registry = cosmon_ops_tools::read_only_registry();
    // Ollama ignores the API key but the OpenAI envelope requires a bearer
    // token field; any non-empty string satisfies it. The provider must
    // ADVERTISE the same read-only ops tools the session DISPATCHES against
    // (`with_tools`) — otherwise the model is never told `observe` /
    // `peek` / `ensemble` exist and falls back to emitting tool calls as
    // plain text.
    let provider = OpenAIProvider::with_base_url("ollama", model, base_url)
        .with_tools(registry.declarations());
    let mut transcript = Transcript::create(&transcript_path)?;

    let config = ReplConfig {
        briefing: BRIEFING,
        work_dir: &work_dir,
        // Local backend: `/observe` reads the local store, the same source
        // as the model's `observe` tool in `read_only_registry`.
        observe: &cosmon_ops_tools::ObserveTool,
    };

    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();

    run_repl(
        provider,
        registry,
        config,
        &mut transcript,
        reader,
        &mut stdout,
    )
    .await?;
    Ok(())
}
