// SPDX-License-Identifier: AGPL-3.0-only

//! `cs pilot` — launch the interactive **cognitive pilot** (REPL) against a
//! client-side model, piloting either the local cosmon instance or a remote
//! avatar.
//!
//! This is the operator-facing front door of the cs-pilot walking skeleton
//! (delib `2026-05-31-cs-pilot-external-cognitive-pilot`, [ADR-115]). It is a
//! human/operator verb in the **Propelled** regime with a single role:
//! foreground-launch the [`cosmon_pilot`] REPL, wiring the default surface
//! — the local Ollama provider plus the read-only [`cosmon_ops_tools`]
//! registry (`observe` / `peek` / `ensemble`, all calling `cosmon-core` /
//! `cosmon-state` directly, no `cs` subprocess). With `--remote` it swaps the
//! tool backend to the §8p HTTP one (see *Remote mode* below) while keeping
//! the loop and model identical. It does not, on either backend, tear
//! anything down; `done` stays its own operator-only verb.
//!
//! ## Experimental gating (mirrors `cs ask`, [ADR-071])
//!
//! Like `cs ask`, the verb is gated behind `--experimental` while the
//! interactive loop matures. Without the flag, `cs pilot` prints a safety
//! notice and exits 0 — **no REPL, no model call, no side effects**. With it,
//! the REPL launches and reads operator lines from stdin until `/quit` or EOF.
//!
//! ## claw-code is bibliography, not vocabulary ([ADR-096])
//!
//! The interactive REPL pattern is borrowed from claw-code as bibliography
//! only. The in-REPL meta-commands are **pilot directives** (not "slash
//! commands"); the conversation artifact is a **transcript** (not a
//! "Session"). No claw vocabulary (`Gateway`, `Sandbox`, `Session`-as-context,
//! `Plugin`, `Channel`, agent-as-daemon) enters the cosmon CLI surface.
//!
//! ## Direct internal-API, no subprocess (local) — or §8p HTTP (remote)
//!
//! Unlike `cs ask` (which shells out to `cs nucleate` + `cs tackle` on
//! `--execute`), `cs pilot` links [`cosmon_pilot`] as a library and drives the
//! REPL in-process. The whole point of cs-pilot is calling the internal API
//! directly; shelling out would re-introduce the mechanical-CRUD limitation
//! the design exists to remove (delib §3).
//!
//! ## Remote mode — one loop, two tool backends (ADR-115 §6, increment 2)
//!
//! `cs pilot --remote [--profile <name>]` keeps the *same* REPL and the
//! *same* client-side model, but swaps the tool backend from the local
//! direct-internal-API tools to the remote
//! [`cosmon_ops_tools::remote`] backend, which calls a `cosmon-rpp-adapter`'s
//! ADR-080 §8p routes via [`cosmon_remote`] (JWT `sub → nucleon_id`). This is
//! how a thin CLI installed *outside* an avatar (e.g. tenant-demo) pilots it over
//! the network: the model runs on the operator's box, only cosmon
//! *operations* cross the wire. Read-only by default (`observe` / `ensemble`);
//! `--write` adds `nucleate` / `tackle`. `done` / `evolve` are **never** on
//! the wire (ADR-080 §5). The JWT is taken from `$COSMON_REMOTE_TOKEN` when
//! set, else minted against the profile's OIDC issuer.
//!
//! [ADR-115]: ../../../../docs/adr/115-cs-pilot-cognitive-pilot.md
//! [ADR-071]: ../../../../docs/adr/071-cs-ask.md
//! [ADR-096]: ../../../../docs/adr/096-openclaw-as-bibliography.md
//! [ADR-080]: ../../../../docs/adr/080-remote-pilot-port-https-oidc.md

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use cosmon_agent_harness::{Tool, ToolRegistry};
use cosmon_pilot::repl::{run_repl, ReplConfig};
use cosmon_pilot::transcript::Transcript;
use cosmon_provider::OpenAIProvider;
use cosmon_remote::{Client, ProfileStore};

use super::Context;

/// Default Ollama OpenAI-compatible endpoint. `OpenAIProvider` normalises a
/// trailing `/v1` away, so either form is accepted (autonomy-local-first).
const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

/// Default local model tag — a small instruct model most Ollama installs
/// already have pulled. Override with `--model` or `COSMON_PILOT_MODEL`.
const DEFAULT_MODEL: &str = "llama3.2";

/// Default per-request timeout, in seconds, for the pilot's model calls.
///
/// Deliberately five minutes — far above the [`OpenAIProvider`] library
/// default of 60s. The pilot is a **local-first** path: it injects the
/// full repo bootstrap context (~36 KB of `CLAUDE.md`) as the opening
/// briefing, so prefill dominates the first round-trip. A large local
/// instruct model with the best tool-calling quality (e.g. `qwen2.5:32b`)
/// legitimately spends ~190s on that first prefill+generation — well past
/// 60s — which silently killed the pilot with `openai http error: error
/// sending request` before this default existed (discovered in the cs-pilot
/// v0 smoke test). Override with `--timeout` or `COSMON_PILOT_TIMEOUT`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// The pilot persona / opening framing the **local** session is seeded with.
/// Kept in sync with the `cosmon-pilot` binary's default briefing.
const BRIEFING: &str = "You are the cosmon pilot — a cognitive co-pilot for an operator running \
     a fleet of AI coding agents. You can inspect the fleet with the read-only \
     tools `observe` (one molecule by id), `peek`, and `ensemble`. When the \
     operator asks about the state of a molecule or the backlog, call the \
     appropriate tool rather than guessing. Be concise.";

/// Briefing for a **remote**, read-only session. No `peek` over the wire.
const BRIEFING_REMOTE_READONLY: &str =
    "You are the cosmon pilot — a cognitive co-pilot driving a REMOTE cosmon \
     avatar over the network. You can inspect its fleet with the read-only tools \
     `observe` (one molecule by id) and `ensemble` (filtered backlog). `peek` is \
     NOT available remotely. When the operator asks about state or the backlog, \
     call the appropriate tool rather than guessing. You cannot create, tackle, \
     or close molecules in this session. Be concise.";

/// Briefing for a **remote** session with write tools enabled (`--write`).
const BRIEFING_REMOTE_WRITE: &str =
    "You are the cosmon pilot — a cognitive co-pilot driving a REMOTE cosmon \
     avatar over the network. You can inspect its fleet with `observe` and \
     `ensemble` (no `peek` remotely), create work with `nucleate`, and dispatch \
     a worker with `tackle`. You can NEVER close, merge, or tear down a molecule \
     — `done` is an operator-only gesture and is not available over the wire; \
     never claim otherwise. Confirm intent before any `nucleate`/`tackle`. Be \
     concise.";

/// Default OIDC scopes minted for a remote pilot session when the profile
/// does not pin its own. Read + write covers the §8p molecule subset; the
/// adapter still refuses operator-only verbs regardless of scope.
const DEFAULT_REMOTE_SCOPES: &[&str] = &["cosmon:molecule:read", "cosmon:molecule:write"];

/// Arguments for `cs pilot`.
#[derive(clap::Args)]
pub struct Args {
    /// Enable the experimental verb. While the interactive loop matures,
    /// `cs pilot` without this flag prints a safety notice and does nothing
    /// — no REPL, no model call, no side effects (mirrors `cs ask`, ADR-071).
    #[arg(long)]
    pub experimental: bool,

    /// Ollama model tag to drive the loop (default: `llama3.2`). Falls back
    /// to the `COSMON_PILOT_MODEL` environment variable when the flag is
    /// omitted.
    #[arg(long, value_name = "TAG")]
    pub model: Option<String>,

    /// Override the model endpoint (default: the local Ollama
    /// OpenAI-compatible endpoint `http://localhost:11434/v1`). Falls back to
    /// `COSMON_PILOT_BASE_URL` when the flag is omitted.
    #[arg(long = "base-url", value_name = "URL")]
    pub base_url: Option<String>,

    /// Path the on-disk transcript is appended to (default:
    /// `pilot-transcript.md` in the current directory). Falls back to
    /// `COSMON_PILOT_TRANSCRIPT` when the flag is omitted.
    #[arg(long, value_name = "PATH")]
    pub transcript: Option<PathBuf>,

    /// Per-request timeout in seconds for each model round-trip (default:
    /// 300). The pilot defaults far above the provider's 60s library
    /// default because it injects the full repo `CLAUDE.md` (~36 KB) as
    /// bootstrap context, so a large local model (e.g. `qwen2.5:32b`)
    /// legitimately spends minutes on the first prefill. Falls back to
    /// `COSMON_PILOT_TIMEOUT` (also in seconds) when the flag is omitted.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Pilot a REMOTE avatar over the network instead of the local cosmon
    /// instance (ADR-115 §6). The model still runs client-side (this box);
    /// only cosmon *operations* cross the wire, via the avatar's
    /// `cosmon-rpp-adapter` §8p routes (ADR-080). Read-only unless `--write`.
    #[arg(long)]
    pub remote: bool,

    /// Which `cosmon-remote` profile to use in `--remote` mode (the avatar's
    /// host + JWT identity). Defaults to the configured default profile
    /// (`~/.config/cosmon-remote/`). Ignored without `--remote`.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// In `--remote` mode, expose the write tools (`nucleate` / `tackle`) in
    /// addition to the read tools. Off by default — a remote session sees the
    /// fleet but cannot change it unless asked. `done` / `evolve` are never
    /// available regardless (ADR-080 §5). Ignored without `--remote`.
    #[arg(long)]
    pub write: bool,
}

/// Entry point for `cs pilot`.
///
/// # Errors
///
/// Returns errors from constructing the tokio runtime, creating the
/// transcript file, or the REPL loop itself (provider failure, context
/// overflow, I/O on the input/output streams). The non-`--experimental`
/// path is infallible and returns `Ok(())`.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    if !args.experimental {
        emit_safety_notice(ctx);
        return Ok(());
    }

    // The REPL is interactive, line-by-line; `--json` has no meaningful
    // shape for a conversational loop. Note it and proceed (the safety
    // notice above is the only JSON-bearing surface of this verb).
    if ctx.json {
        eprintln!("note: --json has no effect on the interactive pilot loop; ignoring.");
    }

    let base_url = args
        .base_url
        .clone()
        .or_else(|| std::env::var("COSMON_PILOT_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let model = args
        .model
        .clone()
        .or_else(|| std::env::var("COSMON_PILOT_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    let transcript_path = match &args.transcript {
        Some(p) => p.clone(),
        None => std::env::var("COSMON_PILOT_TRANSCRIPT")
            .map_or_else(|_| PathBuf::from("pilot-transcript.md"), PathBuf::from),
    };
    let timeout = Duration::from_secs(resolve_timeout_secs(
        args.timeout,
        std::env::var("COSMON_PILOT_TIMEOUT").ok().as_deref(),
    ));

    // A current-thread runtime is sufficient: the REPL blocks on synchronous
    // stdin reads between model round-trips, so there is no concurrent work
    // for a multi-thread scheduler to do. `enable_all` gives the provider's
    // HTTP client its reactor.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let remote = args.remote;
    let profile = args.profile.clone();
    let write = args.write;

    rt.block_on(async move {
        let work_dir = std::env::current_dir()?;

        // Pick the tool BACKEND (the seam ADR-115 §6 rests on): local
        // direct-internal-API tools, or the remote backend over the §8p
        // wire. The REPL and model are identical either way — only these two
        // values differ. `observe` is boxed so the `/observe` directive
        // dispatches through the *same* backend the model uses.
        let (registry, observe): (ToolRegistry, Box<dyn Tool>) = if remote {
            let backend = resolve_remote_backend(profile.as_deref()).await?;
            let registry = if write {
                cosmon_ops_tools::remote_registry(backend.clone())
            } else {
                cosmon_ops_tools::remote_read_only_registry(backend.clone())
            };
            let observe: Box<dyn Tool> = Box::new(cosmon_ops_tools::RemoteObserveTool(backend));
            (registry, observe)
        } else {
            (
                cosmon_ops_tools::read_only_registry(),
                Box::new(cosmon_ops_tools::ObserveTool),
            )
        };

        let briefing = if remote {
            if write {
                BRIEFING_REMOTE_WRITE
            } else {
                BRIEFING_REMOTE_READONLY
            }
        } else {
            BRIEFING
        };

        // Ollama ignores the API key but the OpenAI envelope requires a
        // bearer-token field; any non-empty string satisfies it. The
        // provider must ADVERTISE the same ops tools the session DISPATCHES
        // against (`with_tools`) — otherwise the model is never told the
        // tools exist. The model runs CLIENT-SIDE in both modes (this is the
        // operator's box); remote mode only routes tool calls over the wire.
        let provider = OpenAIProvider::with_base_url("ollama", model, base_url)
            .with_tools(registry.declarations())
            .with_timeout(timeout);
        let mut transcript = Transcript::create(&transcript_path)?;

        let config = ReplConfig {
            briefing,
            work_dir: &work_dir,
            observe: observe.as_ref(),
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
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
    })
}

/// Resolve the per-request timeout (in seconds) from the precedence chain
/// `--timeout` flag → `COSMON_PILOT_TIMEOUT` env → [`DEFAULT_TIMEOUT_SECS`].
///
/// Extracted as a pure function (flag + env string in, seconds out) so the
/// precedence and the malformed-env fallback are unit-testable without
/// touching the process environment or launching the REPL. A malformed env
/// value emits a `note:` to stderr and falls back to the default rather than
/// silently collapsing to the provider's 60s library default — that 60s
/// default is precisely the bug this knob exists to fix.
fn resolve_timeout_secs(flag: Option<u64>, env: Option<&str>) -> u64 {
    if let Some(secs) = flag {
        return secs;
    }
    let Some(raw) = env else {
        return DEFAULT_TIMEOUT_SECS;
    };
    if let Ok(secs) = raw.parse::<u64>() {
        secs
    } else {
        eprintln!(
            "note: COSMON_PILOT_TIMEOUT='{raw}' is not a valid integer number of seconds; using default {DEFAULT_TIMEOUT_SECS}s."
        );
        DEFAULT_TIMEOUT_SECS
    }
}

/// Resolve a [`cosmon_ops_tools::RemoteBackend`] for `--remote` mode.
///
/// Loads the named (or default) `cosmon-remote` profile, then obtains a
/// bearer JWT: `$COSMON_REMOTE_TOKEN` verbatim when set (the CI / smoke
/// path), otherwise minted against the profile's OIDC issuer with the
/// profile's scopes (defaulting to read+write — the adapter still refuses
/// operator-only verbs regardless of scope). The minting round-trip is the
/// only network call made avatar-side from the pilot's own host before the
/// loop starts.
async fn resolve_remote_backend(
    profile_flag: Option<&str>,
) -> Result<cosmon_ops_tools::RemoteBackend> {
    let store = ProfileStore::default_location()
        .context("could not locate the cosmon-remote config directory")?;
    let (name, profile) = store
        .resolve(profile_flag)
        .context("could not resolve a cosmon-remote profile for --remote")?;

    let token = match std::env::var("COSMON_REMOTE_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            let scopes: Vec<String> = if profile.scopes.is_empty() {
                DEFAULT_REMOTE_SCOPES
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            } else {
                profile.scopes.clone()
            };
            let client = Client::new_unchecked(&profile, None)
                .with_context(|| format!("could not build a client for profile {name:?}"))?;
            let minted = client.mint_jwt(&scopes).await.with_context(|| {
                format!(
                    "could not mint a JWT for profile {name:?}. Set $COSMON_REMOTE_TOKEN, \
                     or ensure the profile's sub/aud/oidc_url are configured \
                     (`cosmon-remote config set …`)."
                )
            })?;
            minted.access_token
        }
    };

    Ok(cosmon_ops_tools::RemoteBackend::new(profile, Some(token)))
}

/// Print the `--experimental` safety notice. With `--json`, emit a single
/// NDJSON line carrying the same information so scripts can detect the gate.
fn emit_safety_notice(ctx: &Context) {
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "command": "pilot",
                "experimental": false,
                "launched": false,
                "notice": "cs pilot is experimental — re-run with --experimental to launch the interactive cognitive pilot."
            })
        );
    } else {
        eprintln!(
            "cs pilot is experimental — re-run with --experimental to launch the interactive cognitive pilot (local Ollama, read-only tools)."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_timeout_secs, DEFAULT_TIMEOUT_SECS};

    /// The explicit `--timeout` flag wins over everything, including a
    /// present env var.
    #[test]
    fn flag_takes_precedence_over_env() {
        assert_eq!(resolve_timeout_secs(Some(42), Some("999")), 42);
        assert_eq!(resolve_timeout_secs(Some(42), None), 42);
    }

    /// With no flag, a well-formed env value is honoured verbatim — this is
    /// the operator's escape hatch for a model even slower than the default.
    #[test]
    fn env_used_when_flag_absent() {
        assert_eq!(resolve_timeout_secs(None, Some("600")), 600);
    }

    /// No flag and no env → the local-first default (well above the
    /// provider's 60s library default, which is the bug this knob fixes).
    #[test]
    fn default_when_neither_present() {
        assert_eq!(resolve_timeout_secs(None, None), DEFAULT_TIMEOUT_SECS);
        assert_eq!(DEFAULT_TIMEOUT_SECS, 300);
    }

    /// A malformed env value falls back to the default rather than the
    /// provider's 60s — silently collapsing to 60s would re-introduce the
    /// timeout that killed the 32b pilot model.
    #[test]
    fn malformed_env_falls_back_to_default() {
        assert_eq!(
            resolve_timeout_secs(None, Some("not-a-number")),
            DEFAULT_TIMEOUT_SECS
        );
        assert_eq!(resolve_timeout_secs(None, Some("")), DEFAULT_TIMEOUT_SECS);
    }
}
