// SPDX-License-Identifier: AGPL-3.0-only

//! `cs cluster` — read and edit the machine-level cluster topology
//! file (`~/.config/cosmon/cluster.toml`, see ADR-066).
//!
//! Three sub-verbs:
//!
//! - `cs cluster show` — pretty-print the resolved config
//! - `cs cluster edit` — open `$EDITOR` on the file, seeding a template
//!   if absent
//! - `cs cluster bootstrap <primary_url>` — fetch
//!   `<primary_url>/cluster` from another device in the cluster and
//!   write it locally (provisioning flow for new devices)
//!
//! This command is **human-only** (ADR-066 §3.1 invariant #2). Workers
//! do not read or write the cluster config — they operate at the
//! galaxy level via `.cosmon/config.toml`.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use anyhow::{anyhow, Context as _};
use cosmon_core::cluster::{template_toml, ClusterConfig};
use cosmon_filestore::resolve_cluster_config_path;

use super::Context;

/// Top-level arguments for `cs cluster`.
#[derive(clap::Args)]
pub struct Args {
    /// Optional path override; defaults to
    /// `$COSMON_CLUSTER_CONFIG` or `~/.config/cosmon/cluster.toml`.
    #[arg(long = "cluster-config", global = true, value_name = "PATH")]
    pub cluster_config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Sub,
}

/// Cluster subcommands.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Print the resolved `cluster.toml` in a human-readable table
    /// (default) or JSON (`--json`).
    Show,
    /// Open `$EDITOR` on the file, seeding a template when absent.
    Edit,
    /// Fetch `<primary_url>/cluster` from a primary device and
    /// overwrite the local file.
    Bootstrap {
        /// Base URL of the primary device's cs-api
        /// (e.g. `http://192.0.2.10:4222`).
        primary_url: String,
        /// Overwrite without prompting (useful for scripts).
        #[arg(long)]
        force: bool,
    },
}

/// Entry point dispatched from `main.rs`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let path = resolve_cluster_config_path(args.cluster_config.as_deref());
    match &args.command {
        Sub::Show => run_show(ctx, &path),
        Sub::Edit => run_edit(&path),
        Sub::Bootstrap { primary_url, force } => run_bootstrap(&path, primary_url, *force),
    }
}

fn run_show(ctx: &Context, path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        if ctx.json {
            println!(
                "{{\"error\":\"not_configured\",\"path\":{:?}}}",
                path.display().to_string()
            );
        } else {
            eprintln!(
                "cluster.toml not found at {}\n\n\
                 Seed it with `cs cluster edit` or provision from a peer:\n  \
                 cs cluster bootstrap http://<primary-ip>:4222",
                path.display()
            );
        }
        return Ok(());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg = ClusterConfig::from_toml_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;

    if ctx.json {
        let json = serde_json::to_string_pretty(&cfg)?;
        println!("{json}");
        return Ok(());
    }

    render_human(&cfg, path);
    Ok(())
}

fn render_human(cfg: &ClusterConfig, path: &std::path::Path) {
    println!("cluster.toml  ({})", path.display());
    println!("schema_version = {}", cfg.schema_version);
    println!();
    println!("[cluster]");
    if let Some(name) = &cfg.cluster.name {
        println!("  name             = {name}");
    }
    if let Some(owner) = &cfg.cluster.owner_nucleon_id {
        println!("  owner_nucleon_id = {owner}");
    }
    if let Some(domain) = &cfg.cluster.tailnet_domain {
        if !domain.is_empty() {
            println!("  tailnet_domain   = {domain}");
        }
    }
    if let Some(ts) = &cfg.cluster.updated_at {
        println!("  updated_at       = {ts}");
    }
    if !cfg.host.is_empty() {
        println!();
        println!("[host.*]  ({} device(s))", cfg.host.len());
        for (key, host) in &cfg.host {
            let ip = host.tailscale_ip.as_deref().unwrap_or("-");
            let hn = host.tailscale_hostname.as_deref().unwrap_or("-");
            let role = host.role.as_deref().unwrap_or("-");
            println!("  {key:<18} {ip:<16} {hn:<32} role={role}");
        }
    }
    println!();
    println!("[surfaces]");
    if let Some(cs_api) = &cfg.surfaces.cs_api {
        print!(
            "  cs_api             host={} port={}",
            cs_api.host, cs_api.port
        );
        if let Some(la) = &cs_api.launchagent {
            print!(" launchagent={la}");
        }
        println!();
        if let Some(url) = cfg.cs_api_base_url() {
            println!("                     → {url}");
        }
    }
    if let Some(m) = &cfg.surfaces.matrix_echo_tick {
        print!("  matrix_echo_tick   host={}", m.host);
        if let Some(r) = &m.room_id {
            print!(" room_id={r}");
        }
        println!();
        if let Some(cred) = &m.credentials_file {
            println!("                     credentials_file={cred}");
        }
    }
    if cfg.apps.mac_pilot_bundle_id.is_some() || cfg.apps.ios_pilot_bundle_id.is_some() {
        println!();
        println!("[apps]");
        if let Some(b) = &cfg.apps.mac_pilot_bundle_id {
            println!("  mac_pilot_bundle_id = {b}");
        }
        if let Some(b) = &cfg.apps.ios_pilot_bundle_id {
            println!("  ios_pilot_bundle_id = {b}");
        }
    }
}

fn run_edit(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir {}", parent.display()))?;
        }
        std::fs::write(path, template_toml())
            .with_context(|| format!("seeding template at {}", path.display()))?;
        eprintln!("seeded template at {}", path.display());
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = StdCommand::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("launching editor {editor:?}"))?;
    if !status.success() {
        return Err(anyhow!("editor {editor:?} exited with status {status}"));
    }
    Ok(())
}

fn run_bootstrap(path: &std::path::Path, primary_url: &str, force: bool) -> anyhow::Result<()> {
    if path.exists() && !force {
        return Err(anyhow!(
            "refusing to overwrite existing {} without --force",
            path.display()
        ));
    }
    let url = format!("{}/cluster", primary_url.trim_end_matches('/'));
    let body = fetch_cluster(&url)?;

    // Validate that the body parses as a cluster config (otherwise the
    // user would silently cache garbage).
    let parsed: ClusterConfig = match parse_fetched(&body) {
        Ok(cfg) => cfg,
        Err(e) => return Err(anyhow!("response from {url} did not parse: {e}")),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    std::fs::write(path, parsed.to_toml_string()?)
        .with_context(|| format!("writing {}", path.display()))?;
    eprintln!(
        "wrote {} ({} host(s), {} surface(s))",
        path.display(),
        parsed.host.len(),
        surface_count(&parsed)
    );
    Ok(())
}

fn surface_count(cfg: &ClusterConfig) -> usize {
    [
        cfg.surfaces.cs_api.is_some(),
        cfg.surfaces.matrix_echo_tick.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count()
}

/// Blocking HTTP GET → String. Uses the std-library-free approach of
/// shelling out to `curl`, mirroring how other cosmon CLI verbs
/// provision themselves on bare machines. If `curl` is unavailable
/// the bootstrap fails with a clear message.
fn fetch_cluster(url: &str) -> anyhow::Result<String> {
    let curl = which_curl().ok_or_else(|| {
        anyhow!("`curl` not found on $PATH; install it or fetch the config manually")
    })?;
    let output = StdCommand::new(curl)
        .args(["-fsS", "--max-time", "10", url])
        .output()
        .with_context(|| format!("invoking curl against {url}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("curl exited with status {}: {err}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| anyhow!("curl output not UTF-8: {e}"))
}

/// Convert the fetched body (JSON from `cs-api`) into a
/// `ClusterConfig`. We tolerate the `{"error": "not_configured"}`
/// shape explicitly for a friendlier message.
fn parse_fetched(body: &str) -> anyhow::Result<ClusterConfig> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| anyhow!("response not JSON: {e}"))?;
    if value.get("error").is_some() {
        return Err(anyhow!(
            "primary returned an error payload: {body}; run `cs cluster edit` on the primary first"
        ));
    }
    let cfg: ClusterConfig =
        serde_json::from_value(value).map_err(|e| anyhow!("response not a cluster config: {e}"))?;
    Ok(cfg)
}

fn which_curl() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("curl");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fetched_accepts_valid_payload() {
        let body = r#"{
          "schema_version": 1,
          "cluster": {"name": "you-local"},
          "host": {},
          "surfaces": {},
          "apps": {}
        }"#;
        let cfg = parse_fetched(body).expect("parses");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.cluster.name.as_deref(), Some("you-local"));
    }

    #[test]
    fn parse_fetched_rejects_error_payload() {
        let body = r#"{"error":"not_configured"}"#;
        let err = parse_fetched(body).unwrap_err();
        assert!(format!("{err}").contains("error payload"));
    }
}
