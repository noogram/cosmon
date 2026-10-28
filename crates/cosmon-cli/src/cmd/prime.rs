// SPDX-License-Identifier: AGPL-3.0-only

//! `cs prime` — re-inject current step context for a running molecule.
//!
//! Called by `SessionStart` hook to re-propel a worker after a context
//! reset, crash recovery, or turn boundary. Outputs the current step
//! context to stdout so Claude picks it up as a system reminder.

use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
use cosmon_state::ops::prime::{prime, PrimeView};
use cosmon_state::{MoleculeFilter, StateStore};

use super::Context;

/// Arguments for the `prime` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to prime (optional — auto-detects from running molecule).
    molecule: Option<String>,
    /// Check hook and prime if work is assigned.
    #[arg(long)]
    hook: bool,
}

/// Execute the `prime` command.
pub fn run(_ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = cosmon_filestore::resolve_state_dir(None);
    let store = FileStore::new(&state_dir);

    let mol = if let Some(ref mol_str) = args.molecule {
        let mol_id = MoleculeId::new(mol_str)?;
        store.load_molecule(&mol_id)?
    } else if args.hook {
        let molecules = store.list_molecules(&MoleculeFilter {
            status: Some(cosmon_core::molecule::MoleculeStatus::Running),
            ..Default::default()
        })?;
        match molecules.first() {
            Some(m) => m.clone(),
            None => return Ok(()),
        }
    } else {
        return Err(anyhow::anyhow!(
            "specify a molecule ID or use --hook to auto-detect"
        ));
    };

    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    match prime(&store, &formulas_dir, &mol.id)? {
        PrimeView::Terminal { .. } => {}
        PrimeView::Context(ctx) => print!("{}", ctx.render()),
    }
    Ok(())
}
