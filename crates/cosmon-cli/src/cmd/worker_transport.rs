// SPDX-License-Identifier: AGPL-3.0-only

//! Application composition for worker hosting; no environment-based UI detection.

use cosmon_core::transport_selection::{
    resolve_transport, TransportConfig, TransportKind, TransportSelection,
};

/// Read the existing project/global surfaces, preserving explicit absence.
pub(super) fn resolve(ctx: &super::Context) -> anyhow::Result<TransportSelection> {
    let project_path = super::resolve_config_from_context(ctx);
    let project = cosmon_filestore::load_project_config(&project_path)?;
    let global = if project.transport.backend.is_some() {
        TransportConfig::default()
    } else {
        let path = super::tackle::global_adapter_config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                #[derive(serde::Deserialize)]
                struct Global {
                    #[serde(default)]
                    transport: TransportConfig,
                }
                toml::from_str::<Global>(&text)?.transport
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                TransportConfig::default()
            }
            Err(error) => return Err(error.into()),
        }
    };
    Ok(resolve_transport(&project.transport, &global)?)
}

/// The typed choice commands composition, even while only one implementation is qualified.
pub(super) fn compose(
    kind: TransportKind,
    socket: &str,
) -> anyhow::Result<cosmon_transport::worker_host::WorkerHost> {
    let path;
    let socket = if kind == TransportKind::Herdr {
        path = std::env::var("HERDR_SOCKET_PATH").map_err(|_| {
            anyhow::anyhow!(
                "Herdr dispatch requires HERDR_SOCKET_PATH pointing at an already-running session"
            )
        })?;
        anyhow::ensure!(
            std::path::Path::new(&path).is_absolute(),
            "HERDR_SOCKET_PATH must be absolute"
        );
        &path
    } else {
        socket
    };
    Ok(cosmon_transport::worker_host::WorkerHost::new(
        kind, socket,
    )?)
}

/// Positive dispatch history survives loss of a process snapshot. Only a later
/// explicit legacy dispatch may restore name-based routing for that worker.
pub(super) fn pinned_workers(
    state_dir: &std::path::Path,
) -> anyhow::Result<std::collections::HashSet<cosmon_core::id::WorkerId>> {
    Ok(cosmon_state::transport_pin::pinned_workers(state_dir)?)
}

/// New Exec pins cannot promise a freeze/thaw roundtrip or accept a TUI resume prompt.
/// Legacy workers retain their established path.
pub(super) fn refuse_interactive_control(
    store: &dyn cosmon_state::StateStore,
    worker: &cosmon_state::WorkerData,
    pinned: &std::collections::HashSet<cosmon_core::id::WorkerId>,
    operation: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(!pinned.contains(&worker.id), "{operation}: recorded Codex Exec dispatch has no interactive continuation contract; missing process identity cannot authorize legacy fallback");
    if let Some(id) = worker
        .current_molecule
        .as_ref()
        .or(worker.frozen_molecule.as_ref())
    {
        match store.load_molecule(id) {
            Ok(molecule) => refuse_exec_control(&molecule, operation)?,
            // Older fleet-only workers may carry a historical molecule pointer
            // without a process snapshot. Do not invent a new transport pin.
            Err(cosmon_core::error::CosmonError::MoleculeNotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn refuse_exec_control(
    molecule: &cosmon_state::MoleculeData,
    operation: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        molecule.process.as_ref().is_none_or(|p| p.transport.is_none()),
        "{operation}: pinned Codex Exec workers have no interactive continuation contract; a new dispatch requires explicit resolution of the existing launch"
    );
    Ok(())
}

/// A Herdr launch must never enter a legacy tmux-name observer or park path.
/// Tmux retains its established observer independently of receipt opt-in.
pub(super) fn is_herdr_process(process: &cosmon_core::process::MoleculeProcess) -> bool {
    process
        .transport
        .is_some_and(|selection| selection.backend == TransportKind::Herdr)
}
