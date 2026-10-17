// SPDX-License-Identifier: AGPL-3.0-only

//! Exact molecule terminal observation and optional read-only attachment.

use cosmon_core::id::MoleculeId;
use cosmon_core::terminal::{
    TerminalBackend, TerminalCapability, TerminalIdentity, TerminalUnavailable,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::Context;

/// Arguments for the additive local terminal-view capability.
#[derive(clap::Args)]
pub struct Args {
    /// Complete molecule ID; prefixes are refused.
    pub molecule_id: String,
    /// Attach this terminal read-only, excluding its size from worker sizing.
    #[arg(long, requires = "expect")]
    pub attach: bool,
    /// Incarnation token returned by a fresh terminal observation.
    #[arg(long, requires = "attach", value_name = "TOKEN")]
    pub expect: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
enum Capability {
    Available {
        capability: TerminalCapability,
        identity: TerminalIdentity,
        expected: String,
    },
    Unknown {
        reason: TerminalUnavailable,
    },
    Unavailable {
        reason: TerminalUnavailable,
    },
}

#[derive(Serialize)]
struct Observation {
    schema_version: u32,
    kind: &'static str,
    id: MoleculeId,
    project_id: cosmon_core::id::ProjectId,
    fleet_id: cosmon_core::id::FleetId,
    project_root: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    #[serde(flatten)]
    capability: Capability,
}

/// Execute a local read or attach without lifecycle, input or persistent writes.
///
/// # Errors
/// Refuses incomplete identities, wrong projects, stale tokens and unsupported terminals.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let project_id = super::require_project_identity(ctx)?;
    let config = super::resolve_config_from_context(ctx).canonicalize()?;
    let project_root = config
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| anyhow::anyhow!("terminal requires a galaxy config"))?
        .to_path_buf();
    let state_dir = ctx.state_dir().canonicalize()?;
    let id = MoleculeId::new(&args.molecule_id)?;
    let store = ctx.store();
    let mol = store.load_molecule(&id)?; // Exact load only; never prefix resolution.
    anyhow::ensure!(mol.id == id, "terminal molecule identity mismatch");
    anyhow::ensure!(
        mol.project_id.as_ref().is_none_or(|p| p == &project_id),
        "terminal molecule project mismatch"
    );
    let backend = cosmon_transport::TmuxBackend::new(super::tmux_socket_name(ctx));
    let capability = capability(&mol, &backend, &project_id, &state_dir)?;
    if args.attach {
        let Capability::Available {
            identity, expected, ..
        } = &capability
        else {
            anyhow::bail!(
                "terminal unavailable: {}",
                serde_json::to_string(&capability)?
            );
        };
        anyhow::ensure!(
            args.expect.as_deref() == Some(expected.as_str()),
            "terminal incarnation token changed"
        );
        // State is checked again immediately before external execution. The adapter
        // then validates kernel witnesses and guards exact targets in tmux's queue.
        let current = store.load_molecule(&id)?;
        anyhow::ensure!(
            current.id == id && current.process == mol.process,
            "terminal process binding changed"
        );
        backend.attach_read_only(identity)?;
    } else {
        let view = Observation {
            schema_version: 1,
            kind: "cosmon.terminal",
            id,
            project_id,
            fleet_id: mol.fleet_id.clone(),
            project_root,
            state_dir,
            capability,
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
    }
    Ok(())
}

fn capability(
    mol: &cosmon_state::MoleculeData,
    backend: &dyn TerminalBackend,
    project: &cosmon_core::id::ProjectId,
    state_dir: &std::path::Path,
) -> anyhow::Result<Capability> {
    let Some(process) = &mol.process else {
        return Ok(Capability::Unavailable {
            reason: TerminalUnavailable::NoProcess,
        });
    };
    let Some(identity) = &process.terminal else {
        return Ok(Capability::Unknown {
            reason: TerminalUnavailable::IdentityUnknown,
        });
    };
    if let Err(reason) = backend.validate_terminal(identity) {
        return Ok(match reason {
            TerminalUnavailable::IdentityUnknown | TerminalUnavailable::BackendUnavailable => {
                Capability::Unknown { reason }
            }
            _ => Capability::Unavailable { reason },
        });
    }
    // Includes the full dispatch record: redispatching the same numeric target
    // and project name still invalidates an old view. This is a consistency token,
    // not a credential or authorization grant.
    let material = serde_json::to_vec(&(project, state_dir, &mol.fleet_id, &mol.id, process))?;
    let expected = format!("sha256:{:x}", Sha256::digest(material));
    Ok(Capability::Available {
        capability: TerminalCapability::ReadOnlyIgnoreSize,
        identity: identity.clone(),
        expected,
    })
}
