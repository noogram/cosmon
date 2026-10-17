// SPDX-License-Identifier: AGPL-3.0-only

//! Exact molecule terminal observation, output-only by default, explicit human IO.

use cosmon_core::id::MoleculeId;
use cosmon_core::terminal::{
    TerminalBackend, TerminalCapability, TerminalIdentity, TerminalInteractionBackend,
    TerminalUnavailable,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::Context;

/// Explicit attachment behavior; omission preserves the v1 output-only contract.
#[derive(Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Output only, with no worker size or environment influence.
    #[default]
    ReadOnly,
    /// Literal human input and output; contributes terminal dimensions.
    Interactive,
}

/// Arguments for the additive local terminal-view capability.
#[derive(clap::Args)]
pub struct Args {
    /// Complete molecule ID; prefixes are refused.
    pub molecule_id: String,
    /// Attach using the selected mode (default: output only, no worker resize).
    #[arg(long, requires = "expect")]
    pub attach: bool,
    /// Explicit input capability; interactive dimensions may resize the worker.
    #[arg(long, value_enum, default_value = "read-only")]
    pub mode: Mode,
    /// Incarnation token returned by a fresh terminal observation.
    #[arg(long, requires = "attach", value_name = "TOKEN")]
    pub expect: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AttachmentCapability {
    ReadOnly(TerminalCapability),
    Interactive(InteractiveCapability),
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum InteractiveCapability {
    InteractiveResize,
}

#[derive(Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
enum Capability {
    Available {
        capability: AttachmentCapability,
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

/// Execute a local observation or explicit attachment without lifecycle writes.
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
    let mut capability = capability(&mol, &backend, &project_id, &state_dir)?;
    if args.mode == Mode::Interactive {
        if let Capability::Available { capability, .. } = &mut capability {
            *capability =
                AttachmentCapability::Interactive(InteractiveCapability::InteractiveResize);
        }
    }
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
            current.id == id
                && current.project_id == mol.project_id
                && current.fleet_id == mol.fleet_id
                && current.process == mol.process,
            "terminal process binding changed"
        );
        match args.mode {
            Mode::ReadOnly => backend.attach_read_only(identity)?,
            Mode::Interactive => backend.attach_interactive(identity)?,
        }
    } else {
        let view = Observation {
            schema_version: 1,
            kind: if args.mode == Mode::Interactive {
                "cosmon.terminal.interactive"
            } else {
                "cosmon.terminal"
            },
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
        capability: AttachmentCapability::ReadOnly(TerminalCapability::ReadOnlyIgnoreSize),
        identity: identity.clone(),
        expected,
    })
}
