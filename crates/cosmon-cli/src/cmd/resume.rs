// SPDX-License-Identifier: AGPL-3.0-only

//! `cs resume` — convenience alias for `cs patrol --propel --molecule <id>`.
//!
//! **Perimeter.** `resume` is a user-facing alias only. It maintains the
//! Propelled regime on a target worker by re-delivering the same
//! propulsion signal that `cs patrol --propel` would emit. It does NOT
//! change state, does NOT advance molecules, and does NOT alter fleet
//! membership; the sole effect is an external nudge through the
//! transport backend. The canonical command remains
//! `cs patrol --propel --molecule <id>` (see ADR-016 Layer A and
//! ADR-052 §CLI delta).
//!
//! After a token limit, crash, or pause a worker may be alive but idle
//! on the `❯` prompt. `resume` sends a standardized signal that agents
//! recognize from the RESILIENCE block in their bootstrap prompt, so
//! they re-check state and continue. Pass `--message` for a custom
//! payload when the default signal is not enough.

use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::DesiredState;

use super::Context;

/// The standard resume signal. A codified message that agents recognize
/// from their RESILIENCE briefing. Uses cosmon's physics vocabulary
/// to be unmistakably a system signal, not a user message.
///
/// Ten billion percent reliable.
const DEFAULT_RESUME_SIGNAL: &str =
    "⚛ COSMON RESUME — session re-energized. Check state, check inbox, continue work.";

/// Arguments for the `resume` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Only resume workers in this fleet.
    #[arg(long)]
    pub fleet: Option<String>,

    /// Only resume this specific agent (by name, across all fleets).
    #[arg(long)]
    pub agent: Option<String>,

    /// Custom resume message (default: standard RESUME signal).
    #[arg(long, short = 'c')]
    pub message: Option<String>,
}

/// Execute the `resume` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let fleet = store.load_fleet()?;

    // Build fleet membership map to filter by fleet.
    let fleet_membership = build_fleet_membership(&state_dir);

    // Discover all fleet backends.
    let project_socket = super::tmux_socket_name(ctx);
    let backends = discover_backends(&state_dir, &project_socket);

    let resume_msg = args.message.as_deref().unwrap_or(DEFAULT_RESUME_SIGNAL);

    let mut nudged: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for worker in fleet.workers.values() {
        if worker.desired != DesiredState::Running {
            continue;
        }

        // Filter by fleet if specified.
        if let Some(ref target_fleet) = args.fleet {
            let worker_fleet = fleet_membership.get(worker.id.as_str());
            if worker_fleet.map(String::as_str) != Some(target_fleet.as_str()) {
                continue;
            }
        }

        // Filter by agent name if specified.
        if let Some(ref target_agent) = args.agent {
            if worker.id.as_str() != target_agent.as_str() {
                continue;
            }
        }

        // Try to send the nudge via all backends.
        let mut sent = false;
        for be in &backends {
            if be.is_alive(&worker.id).unwrap_or(false)
                && be.send_input(&worker.id, resume_msg).is_ok()
            {
                // Brief pause then extra Enter to submit pasted text.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = be.send_input(&worker.id, "");
                sent = true;
                break;
            }
        }

        if sent {
            nudged.push(worker.id.as_str().to_owned());
        } else {
            skipped.push(worker.id.as_str().to_owned());
        }
    }

    if ctx.json {
        let out = serde_json::json!({
            "command": "resume",
            "nudged": nudged,
            "skipped": skipped,
        });
        println!("{out}");
    } else {
        println!(
            "Resumed {} worker(s), {} skipped",
            nudged.len(),
            skipped.len()
        );
        for name in &nudged {
            println!("  ✓ {name}");
        }
        for name in &skipped {
            println!("  ~ {name} (unreachable)");
        }
    }

    Ok(())
}

/// Build fleet membership map from deployed fleet specs.
fn build_fleet_membership(
    state_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let mut membership = std::collections::HashMap::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        let fleet_name = spec["name"].as_str().unwrap_or("?").to_owned();
                        if let Some(agents) = spec["agents"].as_array() {
                            for agent in agents {
                                if let Some(name) = agent["name"].as_str() {
                                    membership.insert(name.to_owned(), fleet_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    membership
}

/// Discover all fleet backends + project socket as fallback.
fn discover_backends(
    state_dir: &std::path::Path,
    project_socket: &str,
) -> Vec<cosmon_transport::TmuxBackend> {
    let mut backends = Vec::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = spec["name"].as_str() {
                            backends.push(cosmon_transport::TmuxBackend::new(name));
                        }
                    }
                }
            }
        }
    }
    // Always try the project socket as fallback.
    backends.push(cosmon_transport::TmuxBackend::new(project_socket));
    backends
}
