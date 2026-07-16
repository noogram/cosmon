// SPDX-License-Identifier: AGPL-3.0-only

//! `cs vllm-mlx` — operator-side pre-flight for the vllm-mlx HTTP sidecar.
//!
//! The local-inference offramp for the 2026-06-15 Claude Code billing flip
//! is a Python sidecar exposing
//! `OpenAI` `/v1/*` and Anthropic `/v1/messages` at `127.0.0.1:8000`.
//! `cosmon-provider::openai` and `cosmon-provider::anthropic` point at
//! the sidecar via `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` (env tier)
//! or `[adapters.openai].base_url` / `[adapters.anthropic].base_url`
//! (config tier). No new Rust crate, no new public types — the seam is
//! the wire schema (synthesis §C5, tolnay §2/§5).
//!
//! Subcommands:
//! - `cs vllm-mlx health` — resolve the configured `openai` adapter's
//!   `base_url`, GET `/v1/models` from it, report the loaded model id
//!   and latency. Used as the **pre-flight check** the briefing's
//!   acceptance criterion §2 demands before any worker dispatch that
//!   depends on local inference.
//!
//! The handler stays a pure read of `.cosmon/config.toml` + process env
//! plus one HTTP `GET`. No state mutation, no event emission, no
//! provider crate dependency — the resolution mirrors
//! `cs config show adapters` exactly so the operator sees the same
//! precedence chain.
//!
//! Governing docs:
//! - [ADR-016](../../../docs/adr/016-autonomy-regimes-and-resident-runtime.md) (no daemon in core)
//! - [ADR-082](../../../docs/adr/082-architecture-baseline.md) (substrate-galaxy obligation)
//! - [ADR-103](../../../docs/adr/103-loop-ownership-axis.md) (loop ownership × runtime ownership)
//! - an internal chronicle (offramp principle)

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use serde::{Deserialize, Serialize};

use cosmon_core::config::{AdapterEntry, AdaptersConfig};

use super::Context;

/// Top-level `cs vllm-mlx` argument bundle.
#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: VllmMlxCommand,
}

#[derive(Subcommand)]
enum VllmMlxCommand {
    /// Probe the configured local-inference endpoint (`/v1/models`).
    Health(HealthArgs),
}

#[derive(ClapArgs)]
pub struct HealthArgs {
    /// Override the resolved base URL — bypass the config / env chain.
    /// Useful for one-off probes against a different host or port.
    #[arg(long)]
    base_url: Option<String>,
    /// Per-request timeout (seconds). Defaults to 5.0 — vllm-mlx is
    /// fast on `/v1/models` (no model load involved), so a short
    /// timeout makes a stuck sidecar loud.
    #[arg(long, default_value_t = 5.0)]
    timeout: f64,
}

/// Default base URL fallback when no config row, no env var, and no
/// `--base-url` override is provided. Matches the `vllm-mlx`
/// `LaunchAgent` template `dev.cosmon.vllm-mlx.plist` default port.
///
/// Note: this is **distinct** from the `OPENAI_DEFAULT_BASE_URL`
/// constant in `cmd/config.rs` (`https://api.openai.com`). The latter
/// is the vendor production URL; this is the local-sidecar fallback,
/// used only when the operator has run nothing and asks for a health
/// probe anyway.
const VLLM_MLX_FALLBACK_URL: &str = "http://127.0.0.1:8000";

/// Compile-time `OPENAI_DEFAULT_BASE_URL`. Kept private here so the
/// "still pointing at vendor" branch of the health printout stays
/// honest — if this string matches the resolved URL, the operator has
/// not configured the local sidecar at all.
const OPENAI_VENDOR_DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// `/v1/models` response envelope (`OpenAI`-compatible). `vllm-mlx`
/// mirrors the `OpenAI` shape verbatim, including the `id` / `object` /
/// `owned_by` triple. We only deserialize what we need to print.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

/// One row in the health-check output.
#[derive(Debug, Serialize)]
struct HealthReport {
    /// Base URL the probe was sent to.
    base_url: String,
    /// Which tier supplied `base_url` — `flag`, `config`, `env`, `default`.
    base_url_source: &'static str,
    /// HTTP outcome — `ok`, `error`, or `down`.
    status: &'static str,
    /// Round-trip latency in milliseconds, if the request returned at all.
    latency_ms: Option<u128>,
    /// Loaded model identifiers reported by `/v1/models`.
    models: Vec<String>,
    /// Free-form detail (HTTP status text, transport error, hint).
    detail: Option<String>,
}

pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.command {
        VllmMlxCommand::Health(h) => run_health(ctx, h),
    }
}

// `Result<()>` is required by the dispatcher in `main.rs` and matches the
// shape of every other handler in `cmd/*.rs`. The early-return through
// `process::exit(2)` makes the result look unconditional `Ok(())` to
// clippy; allow it locally rather than reshape the dispatch table for
// one verb. Exit code 2 specifically (not 1) so scripts can tell
// "sidecar down" apart from "argument parse error".
#[allow(clippy::unnecessary_wraps)]
fn run_health(ctx: &Context, args: &HealthArgs) -> Result<()> {
    // Same load path as `cs config show adapters` + `cs tackle`.
    // A missing or unparseable file is silently treated as
    // "no config" — health probes must work on a fresh galaxy.
    let config_path = cosmon_filestore::resolve_config_path(ctx.config.as_deref());
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    let adapters_cfg = project_config.adapters.as_ref();

    let (base_url, base_url_source) =
        resolve_openai_base_url(adapters_cfg, args.base_url.as_deref());
    let timeout = Duration::from_secs_f64(args.timeout.max(0.1));
    let report = probe(&base_url, base_url_source, timeout);
    render(ctx, &report);

    if matches!(report.status, "ok") {
        Ok(())
    } else {
        // Loud non-zero exit so shell users can `cs vllm-mlx health && cs tackle ...`.
        std::process::exit(2);
    }
}

/// Resolve the base URL for the `openai` adapter mirroring
/// `cmd::config::resolve_openai` precedence: `--base-url` flag >
/// `[adapters.openai].base_url` > `OPENAI_BASE_URL` env > local-sidecar
/// fallback (`http://127.0.0.1:8000`).
///
/// Diverges from `cmd::config::resolve_openai` in the fallback: when no
/// signal is present, the health probe assumes the operator wants to
/// check the local sidecar (the only thing this command exists for).
/// `cs config show adapters` would print the vendor default instead —
/// the two commands serve different intents.
fn resolve_openai_base_url(
    adapters_cfg: Option<&AdaptersConfig>,
    flag: Option<&str>,
) -> (String, &'static str) {
    if let Some(url) = flag.filter(|s| !s.is_empty()) {
        return (url.to_owned(), "flag");
    }
    let entry: Option<&AdapterEntry> = adapters_cfg.and_then(|c| c.entry("openai"));
    if let Some(url) = entry.and_then(|e| e.base_url.clone()) {
        return (url, "config");
    }
    if let Some(url) = std::env::var("OPENAI_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return (url, "env");
    }
    (VLLM_MLX_FALLBACK_URL.to_owned(), "default")
}

/// One HTTP GET against `<base_url>/v1/models`. The handler is sync
/// (`reqwest::blocking`) so the CLI does not pull a tokio runtime in.
fn probe(base_url: &str, base_url_source: &'static str, timeout: Duration) -> HealthReport {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return HealthReport {
                base_url: base_url.to_owned(),
                base_url_source,
                status: "error",
                latency_ms: None,
                models: Vec::new(),
                detail: Some(format!("http client build failed: {e}")),
            };
        }
    };

    let started = Instant::now();
    match client.get(&url).send() {
        Ok(resp) => {
            let latency = started.elapsed().as_millis();
            let status_code = resp.status();
            if !status_code.is_success() {
                let body = resp.text().unwrap_or_default();
                return HealthReport {
                    base_url: base_url.to_owned(),
                    base_url_source,
                    status: "error",
                    latency_ms: Some(latency),
                    models: Vec::new(),
                    detail: Some(format!("HTTP {status_code}: {}", truncate(&body, 200))),
                };
            }
            match resp.json::<ModelsResponse>() {
                Ok(parsed) => HealthReport {
                    base_url: base_url.to_owned(),
                    base_url_source,
                    status: "ok",
                    latency_ms: Some(latency),
                    models: parsed
                        .data
                        .into_iter()
                        .map(|m| {
                            if let Some(o) = m.owned_by {
                                format!("{} (owned_by={o})", m.id)
                            } else {
                                m.id
                            }
                        })
                        .collect(),
                    detail: None,
                },
                Err(e) => HealthReport {
                    base_url: base_url.to_owned(),
                    base_url_source,
                    status: "error",
                    latency_ms: Some(latency),
                    models: Vec::new(),
                    detail: Some(format!("decode /v1/models response: {e}")),
                },
            }
        }
        Err(e) => {
            let hint = if base_url == OPENAI_VENDOR_DEFAULT_BASE_URL {
                Some("base_url is the vendor default — run scripts/install-vllm-mlx-launchagent.sh, or set OPENAI_BASE_URL=http://127.0.0.1:8000".to_owned())
            } else {
                None
            };
            HealthReport {
                base_url: base_url.to_owned(),
                base_url_source,
                status: "down",
                latency_ms: None,
                models: Vec::new(),
                detail: hint.or_else(|| Some(e.to_string())),
            }
        }
    }
}

fn render(ctx: &Context, r: &HealthReport) {
    if ctx.json {
        // SAFETY: HealthReport is a `Serialize` struct with no
        // non-serializable fields; serde_json::to_string cannot fail
        // here in practice. Fall back to a hand-written shape if it
        // ever does, rather than failing the command.
        let line = serde_json::to_string(r).unwrap_or_else(|_| {
            r#"{"status":"error","detail":"serde_json::to_string failed"}"#.to_owned()
        });
        println!("{line}");
        return;
    }

    let lat = r
        .latency_ms
        .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms"));
    let label = match r.status {
        "ok" => "OK",
        "error" => "ERR",
        "down" => "DOWN",
        other => other,
    };
    println!(
        "vllm-mlx: {label}  base_url={url} ({src})  latency={lat}",
        url = r.base_url,
        src = r.base_url_source,
    );
    if !r.models.is_empty() {
        println!("  models:");
        for m in &r.models {
            println!("    - {m}");
        }
    }
    if let Some(d) = &r.detail {
        if !d.trim().is_empty() {
            println!("  └─ {}", truncate(d, 160));
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out = s.chars().take(n).collect::<String>();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::config::{AdapterEntry, AdaptersConfig};
    use std::collections::BTreeMap;

    fn empty_cfg() -> AdaptersConfig {
        AdaptersConfig::default()
    }

    fn cfg_with_openai_base(url: &str) -> AdaptersConfig {
        let mut entries = BTreeMap::new();
        entries.insert(
            "openai".to_owned(),
            AdapterEntry {
                base_url: Some(url.to_owned()),
                ..AdapterEntry::default()
            },
        );
        AdaptersConfig {
            default: None,
            entries,
        }
    }

    /// One serial test covers all four precedence tiers — `flag`,
    /// `config`, `env`, `default` — because they share the
    /// `OPENAI_BASE_URL` process env var. Cargo runs tests in the same
    /// binary in parallel by default; splitting these into four tests
    /// produces real races between them (observed: env-set in test B
    /// is visible to the "default" assertion in test A). Sharing one
    /// `#[test]` and owning the env var for the whole function is the
    /// minimal-blast-radius fix — no `serial_test` dep, no global
    /// mutex, no `--test-threads=1` discipline carried into the
    /// workspace suite.
    #[test]
    fn resolve_openai_base_url_precedence_chain() {
        let cfg_with_config = cfg_with_openai_base("http://config:9999");
        let cfg_empty = empty_cfg();

        let saved = std::env::var("OPENAI_BASE_URL").ok();
        // SAFETY: serial test that owns the env var across all assertions.
        unsafe {
            std::env::remove_var("OPENAI_BASE_URL");
        }

        // Tier 1 — flag beats every other source.
        let (url, src) = resolve_openai_base_url(Some(&cfg_with_config), Some("http://flag:1234"));
        assert_eq!(url, "http://flag:1234");
        assert_eq!(src, "flag");

        // Tier 2 — config beats env beats default.
        unsafe {
            std::env::set_var("OPENAI_BASE_URL", "http://env:8888");
        }
        let (url, src) = resolve_openai_base_url(Some(&cfg_with_config), None);
        assert_eq!(url, "http://config:9999");
        assert_eq!(src, "config");

        // Tier 3 — env wins when config is empty.
        let (url, src) = resolve_openai_base_url(Some(&cfg_empty), None);
        assert_eq!(url, "http://env:8888");
        assert_eq!(src, "env");

        // Tier 4 — local-sidecar default when nothing else is set.
        unsafe {
            std::env::remove_var("OPENAI_BASE_URL");
        }
        let (url, src) = resolve_openai_base_url(None, None);
        assert_eq!(url, VLLM_MLX_FALLBACK_URL);
        assert_eq!(src, "default");

        // Restore the operator's env var so other tests aren't surprised.
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("OPENAI_BASE_URL", v);
            }
        }
    }

    #[test]
    fn probe_reports_down_on_unreachable_endpoint() {
        // 127.0.0.1:1 is reserved and never bound — connect refuses.
        let report = probe("http://127.0.0.1:1", "default", Duration::from_millis(200));
        assert_eq!(report.status, "down");
        assert_eq!(report.latency_ms, None);
        assert!(report.detail.is_some());
    }

    #[test]
    fn truncate_does_not_panic_on_multibyte() {
        let s = "🎯🎯🎯🎯🎯";
        assert!(!truncate(s, 3).is_empty());
    }
}
