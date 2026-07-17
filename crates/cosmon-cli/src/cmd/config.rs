// SPDX-License-Identifier: AGPL-3.0-only

//! `cs config` — operator view over `.cosmon/config.toml` resolution.
//!
//! The intent is **debug the silent-leak trap**: when an `openai`-named
//! Adapter is free-ridden onto xAI / Moonshot / `DeepSeek` (override
//! `base_url`, swap `api_key_env`), the operator needs a single command
//! that prints the *effective* resolution — which env var actually holds
//! the credential, which URL the next `cs tackle` will POST against,
//! which model identifier will be sent — before they kick off a worker
//! that silently routes to the wrong vendor.
//!
//! See an internal chronicle
//! for the failure mode that motivated this command.
//!
//! Subcommands:
//!
//! - `cs config show adapters` — for every Direct-API adapter (`openai`,
//!   `anthropic`), print the resolved `api_key_env` / `base_url` /
//!   `default_model`, indicating for each which tier won (`config` / `env` /
//!   `default`) and whether the key env var is actually set.
//! - `cs config adapters` — list every adapter name the dispatch registry
//!   would accept right now (the union of compile-time built-ins and
//!   `[adapters.<name>]` rows in `.cosmon/config.toml`). This is the
//!   discoverability counterpart of `cs config show adapters`: the latter
//!   answers *"if I tackle as openai, where will the bytes go?"*, the former
//!   answers *"what names is the validator currently willing to accept?"*.
//!   See ADR-097 for the registry-projection doctrine, and the API
//!   minimalism rule on versioned wire envelopes
//!   for the `cs.adapters.list/v1` `--json` shape.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;

use cosmon_core::config::{AdapterEntry, AdaptersConfig};
use cosmon_core::spawn_seam::built_in_adapter_names;

use super::Context;

/// Top-level `cs config` argument bundle.
#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show resolved configuration for a topic (currently: `adapters`).
    Show(ShowArgs),
    /// List every adapter name the dispatch registry would accept (union
    /// of compile-time built-ins and `[adapters.<name>]` rows from
    /// `.cosmon/config.toml`).
    Adapters,
}

#[derive(ClapArgs)]
pub struct ShowArgs {
    #[command(subcommand)]
    topic: ShowTopic,
}

#[derive(Subcommand)]
enum ShowTopic {
    /// Print the effective `[adapters.*]` resolution for Direct-API
    /// adapters (`openai`, `anthropic`).
    Adapters,
}

/// Three-tier resolution provenance — which source supplied the value.
///
/// Mirrors the docstring on `spawn_openai_session` (and the matching
/// Anthropic branch): `config` is `.cosmon/config.toml`, `env` is the
/// process environment, `default` is the compile-time fallback baked
/// into the provider crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    Config,
    Env,
    Default,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Config => "config",
            Source::Env => "env",
            Source::Default => "default",
        }
    }
}

/// One row in the `cs config show adapters` table — the resolved view
/// for one Direct-API adapter.
#[derive(Debug, Serialize)]
struct AdapterRow {
    /// Adapter name (`"openai"` / `"anthropic"`).
    adapter: String,
    /// Env var read for the API key.
    api_key_env: String,
    /// Which tier supplied `api_key_env` — `config` (explicit row),
    /// `env` (historical scan picked it up), or `default` (compile-time
    /// fallback).
    api_key_source: Source,
    /// Whether the named env var is set and non-empty *right now*. This
    /// is the trap detector — a `config`-tier `XAI_API_KEY` that is
    /// unset blocks dispatch with a loud error; a `default`-tier
    /// `OPENAI_API_KEY` that is unexpectedly set is the silent-leak
    /// signal the operator came here to see.
    api_key_present: bool,
    /// Resolved base URL the next `cs tackle` would POST against.
    base_url: String,
    /// Provenance for `base_url`.
    base_url_source: Source,
    /// Resolved model identifier the next `cs tackle` would send.
    default_model: String,
    /// Provenance for `default_model`.
    default_model_source: Source,
}

/// `openai`-name compile-time defaults, mirroring
/// `crates/cosmon-provider/src/openai.rs`. Hard-coded here rather than
/// re-exported so this command stays a pure read of `config.toml` +
/// process env — no provider crate dependency.
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com";
const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";
/// Historical multi-vendor scan order for the `openai` adapter — first
/// non-empty wins. Must stay in sync with `openai_credentials` in
/// `crates/cosmon-cli/src/cmd/tackle.rs`.
const OPENAI_ENV_SCAN: &[(&str, Option<&str>)] = &[
    ("OPENAI_API_KEY", None),
    ("XAI_API_KEY", Some("https://api.x.ai")),
    ("MOONSHOT_API_KEY", Some("https://api.moonshot.ai")),
];

const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Fallback model for the `anthropic` Direct-API adapter when neither
/// `[adapters.anthropic].default_model` nor `ANTHROPIC_MODEL` is set.
/// Single named home of this literal in the crate (classe aec8:
/// duplicated copies drift at the next model) — `cmd::tackle`'s
/// `spawn_anthropic_session` references this const. Note the tenant
/// claude-session pin (avatar-surface D1) is a different mechanism:
/// it lives in the rpp-adapter instance config (`claude_model` in
/// `rpp.toml`) and reaches workers via the `ANTHROPIC_MODEL` env
/// export, which this fallback sits *below* in the precedence chain.
pub(crate) const ANTHROPIC_DEFAULT_MODEL: &str = "claude-opus-4-7";
const ANTHROPIC_DEFAULT_KEY_ENV: &str = "ANTHROPIC_API_KEY";

pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.command {
        ConfigCommand::Show(s) => match &s.topic {
            ShowTopic::Adapters => run_show_adapters(ctx),
        },
        ConfigCommand::Adapters => run_list_adapters(ctx),
    }
}

/// Wire envelope for `cs config adapters --json`.
///
/// The `schema` field is a versioned identifier (`cs.adapters.list/v1`)
/// per tolnay's API-minimalism discipline on stable wire formats: a
/// downstream pipeline pins on the slug, and a breaking change must
/// bump the version rather than silently mutate the shape. The
/// envelope is a single JSON object (not NDJSON) — `cs config adapters`
/// is a one-shot snapshot, not a stream.
#[derive(Debug, Serialize)]
struct AdaptersListEnvelope {
    /// `cs.adapters.list/v1` — stable wire slug. Pinned by the
    /// `json_envelope_schema_is_stable` test so a rename breaks the
    /// build, not a downstream consumer.
    schema: &'static str,
    /// Path of the config file that was consulted for the TOML half of
    /// the union. Always present even when the file does not exist
    /// (the registry then collapses to the built-in set alone).
    config_path: String,
    /// One row per accepted adapter name, sorted lexicographically so
    /// the output is diff-friendly and bisectable.
    adapters: Vec<AdapterListRow>,
}

/// One row in the `cs config adapters` projection — a single accepted
/// adapter name with its provenance flags.
///
/// `built_in` and `toml` are independent booleans (not a discriminated
/// enum) because the union semantics are honest: a name can be in
/// both sets simultaneously — `openai` is a built-in *and* often
/// carries a `[adapters.openai]` row overriding its `api_key_env` /
/// `base_url` / `default_model`. Folding these into one `source` field
/// would lose that distinction.
#[derive(Debug, Serialize)]
struct AdapterListRow {
    /// Adapter name as it would be passed to `cs tackle --adapter <name>`.
    name: String,
    /// True iff the name is shipped in-tree via
    /// [`built_in_adapter_names`].
    built_in: bool,
    /// True iff the name appears as a `[adapters.<name>]` row in
    /// `.cosmon/config.toml`.
    toml: bool,
}

/// Stable wire slug for the `cs config adapters --json` envelope.
///
/// Lifted to a `const` so the test pinning the value reads identically
/// to the code emitting it. Bump the `/vN` suffix on any breaking
/// change to the envelope shape.
const ADAPTERS_LIST_SCHEMA: &str = "cs.adapters.list/v1";

/// Compute the registry projection: union of built-in adapter names and
/// TOML-declared adapter names, sorted lexicographically.
///
/// Lifted to a free helper so the unit tests can exercise the
/// projection without round-tripping through `cs` (the projection is
/// the load-bearing logic; the rest is plumbing). Mirrors the
/// `declared_names` composition in [`super::tackle::run`] — the two must
/// agree by construction, which is why both consume
/// [`built_in_adapter_names`] and [`AdaptersConfig::available_names`]
/// rather than maintaining parallel lists.
fn registry_projection(adapters_cfg: Option<&AdaptersConfig>) -> Vec<AdapterListRow> {
    use std::collections::BTreeMap;

    let mut rows: BTreeMap<String, AdapterListRow> = BTreeMap::new();
    for name in built_in_adapter_names() {
        rows.insert(
            (*name).to_owned(),
            AdapterListRow {
                name: (*name).to_owned(),
                built_in: true,
                toml: false,
            },
        );
    }
    if let Some(cfg) = adapters_cfg {
        for owned_name in cfg.available_names() {
            rows.entry(owned_name.clone())
                .and_modify(|r| r.toml = true)
                .or_insert(AdapterListRow {
                    name: owned_name,
                    built_in: false,
                    toml: true,
                });
        }
    }
    rows.into_values().collect()
}

fn run_list_adapters(ctx: &Context) -> Result<()> {
    let config_path = cosmon_filestore::resolve_config_path(ctx.config.as_deref());
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    let rows = registry_projection(project_config.adapters.as_ref());

    if ctx.json {
        let envelope = AdaptersListEnvelope {
            schema: ADAPTERS_LIST_SCHEMA,
            config_path: config_path.display().to_string(),
            adapters: rows,
        };
        println!("{}", serde_json::to_string(&envelope)?);
        return Ok(());
    }

    println!("{}", config_path.display());
    println!();
    println!("{:<14} {:<10} {:<6}", "ADAPTER", "BUILT_IN", "TOML");
    println!("{}", "-".repeat(34));
    for r in &rows {
        println!(
            "{:<14} {:<10} {:<6}",
            r.name,
            if r.built_in { "yes" } else { "no" },
            if r.toml { "yes" } else { "no" },
        );
    }
    Ok(())
}

fn run_show_adapters(ctx: &Context) -> Result<()> {
    // Same load path as `cs tackle` (resolve_config_path + load_project_config);
    // an absent or unparseable file is silently treated as "no config", so
    // this command still works on a fresh galaxy.
    let config_path = cosmon_filestore::resolve_config_path(ctx.config.as_deref());
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    let adapters_cfg = project_config.adapters.as_ref();

    let rows = vec![
        resolve_openai(adapters_cfg),
        resolve_anthropic(adapters_cfg),
    ];

    if ctx.json {
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }

    println!("{}", config_path.display());
    println!();
    println!(
        "{:<10} {:<20} {:<7} {:<7} {:<32} {:<7} {:<22} {:<7}",
        "ADAPTER", "API_KEY_ENV", "SRC", "SET?", "BASE_URL", "SRC", "MODEL", "SRC"
    );
    println!("{}", "-".repeat(120));
    for r in &rows {
        println!(
            "{:<10} {:<20} {:<7} {:<7} {:<32} {:<7} {:<22} {:<7}",
            r.adapter,
            r.api_key_env,
            r.api_key_source.as_str(),
            if r.api_key_present { "yes" } else { "NO" },
            r.base_url,
            r.base_url_source.as_str(),
            r.default_model,
            r.default_model_source.as_str(),
        );
    }
    Ok(())
}

/// Resolve the effective `openai` row. Mirrors the precedence rules in
/// `spawn_openai_session` and `openai_credentials` in
/// `crates/cosmon-cli/src/cmd/tackle.rs`. Reads env vars verbatim — the
/// caller has already loaded the config table.
fn resolve_openai(adapters_cfg: Option<&AdaptersConfig>) -> AdapterRow {
    let entry: Option<&AdapterEntry> = adapters_cfg.and_then(|c| c.entry("openai"));

    let (api_key_env, api_key_source) =
        if let Some(env_name) = entry.and_then(|e| e.api_key_env.as_deref()) {
            (env_name.to_owned(), Source::Config)
        } else {
            // Historical scan: surface the first key that *is* set; if
            // none is set, fall back to OPENAI_API_KEY as the "what we
            // would have tried first" hint.
            let scanned = OPENAI_ENV_SCAN
                .iter()
                .find(|(name, _)| std::env::var(name).is_ok_and(|v| !v.is_empty()))
                .map(|(name, _)| (*name).to_owned());
            match scanned {
                Some(name) => (name, Source::Env),
                None => ("OPENAI_API_KEY".to_owned(), Source::Default),
            }
        };
    let api_key_present = std::env::var(&api_key_env).is_ok_and(|v| !v.is_empty());

    // base_url precedence: config > env OPENAI_BASE_URL > vendor default
    // associated with the scanned env-var (when api_key_source == Env)
    // > compile-time default.
    let (base_url, base_url_source) = if let Some(url) = entry.and_then(|e| e.base_url.clone()) {
        (url, Source::Config)
    } else if let Some(url) = std::env::var("OPENAI_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
    {
        (url, Source::Env)
    } else {
        let vendor_default = OPENAI_ENV_SCAN
            .iter()
            .find(|(name, _)| name == &api_key_env.as_str())
            .and_then(|(_, url)| url.map(ToOwned::to_owned));
        match vendor_default {
            // Hard-coded vendor URLs (api.x.ai, api.moonshot.ai) are
            // baked into the env-scan table — provenance is `env`
            // because they come from "you set XAI_API_KEY in the
            // shell", not from a config row.
            Some(url) => (url, Source::Env),
            None => (OPENAI_DEFAULT_BASE_URL.to_owned(), Source::Default),
        }
    };

    let (default_model, default_model_source) =
        if let Some(m) = entry.and_then(|e| e.default_model.clone()) {
            (m, Source::Config)
        } else if let Some(m) = std::env::var("OPENAI_MODEL").ok().filter(|s| !s.is_empty()) {
            (m, Source::Env)
        } else {
            (OPENAI_DEFAULT_MODEL.to_owned(), Source::Default)
        };

    AdapterRow {
        adapter: "openai".to_owned(),
        api_key_env,
        api_key_source,
        api_key_present,
        base_url,
        base_url_source,
        default_model,
        default_model_source,
    }
}

/// Resolve the effective `anthropic` row. Single-vendor — no free-rider
/// scan; the env-tier default is the literal `ANTHROPIC_API_KEY`.
fn resolve_anthropic(adapters_cfg: Option<&AdaptersConfig>) -> AdapterRow {
    let entry: Option<&AdapterEntry> = adapters_cfg.and_then(|c| c.entry("anthropic"));

    let (api_key_env, api_key_source) = match entry.and_then(|e| e.api_key_env.as_deref()) {
        Some(env_name) => (env_name.to_owned(), Source::Config),
        None => (ANTHROPIC_DEFAULT_KEY_ENV.to_owned(), Source::Default),
    };
    let api_key_present = std::env::var(&api_key_env).is_ok_and(|v| !v.is_empty());

    let (base_url, base_url_source) = if let Some(url) = entry.and_then(|e| e.base_url.clone()) {
        (url, Source::Config)
    } else if let Some(url) = std::env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
    {
        (url, Source::Env)
    } else {
        (ANTHROPIC_DEFAULT_BASE_URL.to_owned(), Source::Default)
    };

    let (default_model, default_model_source) =
        if let Some(m) = entry.and_then(|e| e.default_model.clone()) {
            (m, Source::Config)
        } else if let Some(m) = std::env::var("ANTHROPIC_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
        {
            (m, Source::Env)
        } else {
            (ANTHROPIC_DEFAULT_MODEL.to_owned(), Source::Default)
        };

    AdapterRow {
        adapter: "anthropic".to_owned(),
        api_key_env,
        api_key_source,
        api_key_present,
        base_url,
        base_url_source,
        default_model,
        default_model_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Source slugs are stable wire labels — `--json` consumers and
    /// downstream tooling pin on these. Lock the strings so a rename
    /// breaks the build, not a CI canary.
    #[test]
    fn source_slugs_are_stable() {
        assert_eq!(Source::Config.as_str(), "config");
        assert_eq!(Source::Env.as_str(), "env");
        assert_eq!(Source::Default.as_str(), "default");
    }

    /// Defaults table mirrors the provider crate's compile-time
    /// constants — a drift here means `cs config show adapters` would
    /// lie to the operator. Centralising the literals in two crates
    /// is intentional (this command must stay a pure read of
    /// `config.toml` + env), so this test is the only enforcement
    /// against silent divergence.
    #[test]
    fn default_constants_match_provider_crate() {
        assert_eq!(OPENAI_DEFAULT_BASE_URL, "https://api.openai.com");
        assert_eq!(OPENAI_DEFAULT_MODEL, "gpt-4o-mini");
        assert_eq!(ANTHROPIC_DEFAULT_BASE_URL, "https://api.anthropic.com");
        assert_eq!(ANTHROPIC_DEFAULT_MODEL, "claude-opus-4-7");
        assert_eq!(ANTHROPIC_DEFAULT_KEY_ENV, "ANTHROPIC_API_KEY");
    }

    /// `OPENAI_ENV_SCAN` must contain every key in the precedence
    /// order used by `openai_credentials` in `tackle.rs`. Drift here
    /// would make `cs config show adapters` report an env var the
    /// dispatch path doesn't actually read.
    #[test]
    fn openai_env_scan_order_matches_dispatch() {
        let names: Vec<&str> = OPENAI_ENV_SCAN.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec!["OPENAI_API_KEY", "XAI_API_KEY", "MOONSHOT_API_KEY"]
        );
    }

    /// `cs.adapters.list/v1` is a stable wire slug — downstream
    /// pipelines `jq -r '.schema'` against it. A rename or version
    /// bump must be intentional, not silent. Pin the literal so the
    /// test diff carries the change.
    #[test]
    fn json_envelope_schema_is_stable() {
        assert_eq!(ADAPTERS_LIST_SCHEMA, "cs.adapters.list/v1");
    }

    /// With no `[adapters]` block in TOML, the projection collapses to
    /// the built-in set — every row marked `built_in: true, toml:
    /// false`, sorted lexicographically.
    #[test]
    fn projection_with_no_toml_block_is_built_ins_only() {
        let rows = registry_projection(None);
        let built_ins = built_in_adapter_names();
        assert_eq!(rows.len(), built_ins.len());
        for r in &rows {
            assert!(r.built_in, "{} should be built_in", r.name);
            assert!(!r.toml, "{} should not be toml", r.name);
            assert!(
                built_ins.contains(&r.name.as_str()),
                "{} not in built-in set",
                r.name
            );
        }
        // Sorted lexicographically.
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    /// A TOML row for a built-in name (the common `[adapters.openai]`
    /// override case) must show up as `built_in: true, toml: true` —
    /// not two separate rows, not a silent drop of the built-in
    /// provenance.
    #[test]
    fn projection_marks_built_in_with_toml_override() {
        let cfg = AdaptersConfig {
            default: None,
            entries: std::collections::BTreeMap::from([(
                "openai".to_owned(),
                AdapterEntry {
                    api_key_env: Some("XAI_API_KEY".to_owned()),
                    base_url: Some("https://api.x.ai".to_owned()),
                    ..Default::default()
                },
            )]),
        };
        let rows = registry_projection(Some(&cfg));
        let openai = rows
            .iter()
            .find(|r| r.name == "openai")
            .expect("openai row present");
        assert!(openai.built_in);
        assert!(openai.toml);
        // No duplicate.
        assert_eq!(rows.iter().filter(|r| r.name == "openai").count(), 1);
    }

    /// A TOML row whose name is not in the built-in set (a
    /// hand-authored installation-perimeter adapter) must surface as
    /// `built_in: false, toml: true` — the validator will accept it
    /// at dispatch time, and `cs config adapters` is the operator's
    /// proof that the row is loaded.
    #[test]
    fn projection_includes_toml_only_names() {
        let cfg = AdaptersConfig {
            default: None,
            entries: std::collections::BTreeMap::from([(
                "internal-llm".to_owned(),
                AdapterEntry::default(),
            )]),
        };
        let rows = registry_projection(Some(&cfg));
        let row = rows
            .iter()
            .find(|r| r.name == "internal-llm")
            .expect("internal-llm row present");
        assert!(!row.built_in);
        assert!(row.toml);
    }

    /// JSON envelope must carry the four load-bearing keys (`schema`,
    /// `config_path`, `adapters`, and `built_in`/`toml`/`name` on each
    /// row). The check is shallow on purpose — anything richer becomes
    /// a maintenance tax on the wire format. Downstream consumers that
    /// pin specific keys belong in their own test suite.
    #[test]
    fn json_envelope_carries_expected_keys() {
        let envelope = AdaptersListEnvelope {
            schema: ADAPTERS_LIST_SCHEMA,
            config_path: "/tmp/none.toml".to_owned(),
            adapters: vec![AdapterListRow {
                name: "claude".to_owned(),
                built_in: true,
                toml: false,
            }],
        };
        let raw = serde_json::to_string(&envelope).expect("serialise envelope");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
        assert_eq!(v["schema"], "cs.adapters.list/v1");
        assert_eq!(v["config_path"], "/tmp/none.toml");
        let adapters = v["adapters"].as_array().expect("adapters is array");
        assert_eq!(adapters.len(), 1);
        let row = &adapters[0];
        assert_eq!(row["name"], "claude");
        assert_eq!(row["built_in"], true);
        assert_eq!(row["toml"], false);
    }
}
