// SPDX-License-Identifier: AGPL-3.0-only

//! `cs prime` — re-inject current step context for a running molecule.
//!
//! Called by `SessionStart` hook to re-propel a worker after a context
//! reset, crash recovery, or turn boundary. Outputs the current step
//! context to stdout so Claude picks it up as a system reminder.

use cosmon_core::formula::Formula;
use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
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
#[allow(clippy::too_many_lines, clippy::comparison_chain)]
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

    if mol.status.is_terminal() {
        return Ok(());
    }

    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id));
    let formula = std::fs::read_to_string(&formula_path)
        .ok()
        .and_then(|t| Formula::parse(&t).ok());

    println!(
        "🚨 PROPULSION REMINDER — You are executing molecule `{}`.\n",
        mol.id
    );

    if let Some(ref formula) = formula {
        println!(
            "**Progress:** step {}/{}\n",
            mol.current_step + 1,
            mol.total_steps
        );
        for (i, step) in formula.steps.iter().enumerate() {
            if i < mol.current_step {
                println!("- [x] Step {}: {}", i + 1, step.title);
            } else if i == mol.current_step {
                println!("- [>] **Step {}: {}** ◀ EXECUTE NOW", i + 1, step.title);
                println!("  {}", step.description);
                if let Some(ref criteria) = step.exit_criteria {
                    println!("  Exit criteria: {criteria}");
                }
            } else {
                println!("- [ ] Step {}: {}", i + 1, step.title);
            }
        }
    } else {
        println!(
            "Step {}/{} — continue execution.",
            mol.current_step + 1,
            mol.total_steps
        );
    }

    println!("\n**Do NOT pause. Execute the current step immediately.**");
    println!(
        "When ALL steps done: `cs complete {} --reason \"<summary>\"`",
        mol.id
    );

    Ok(())
}
