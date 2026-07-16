// SPDX-License-Identifier: AGPL-3.0-only

//! `cs claim` / `cs release` — pilot ownership of a pending molecule.
//!
//! A claim is a durable, idempotent `hold:pilot` tag. The resident runtime
//! reads that marker immediately before dispatch and defers unconditionally.

use cosmon_core::id::MoleculeId;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::ops::{tag, TagError, TagJson};

use super::Context;

const PILOT_HOLD: &str = "hold:pilot";

/// Arguments shared by the claim and release verbs.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to claim or release.
    pub molecule_id: String,
}

/// Persist a pilot reservation before manually tackling a molecule.
pub fn claim(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    change(ctx, &args.molecule_id, true)
}

/// Remove a pilot reservation and return the molecule to the runtime frontier.
pub fn release(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    change(ctx, &args.molecule_id, false)
}

fn change(ctx: &Context, raw_id: &str, claim: bool) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);
    let mol_id = MoleculeId::new(raw_id).map_err(|e| anyhow::anyhow!("invalid id: {e}"))?;
    let hold = Tag::new(PILOT_HOLD).expect("pilot hold tag is valid");
    let add = claim
        .then_some(hold.clone())
        .into_iter()
        .collect::<Vec<_>>();
    let remove = (!claim).then_some(hold).into_iter().collect::<Vec<_>>();
    let delta =
        tag(&store, &state_dir, "operator", &mol_id, &add, &remove).map_err(|e| match e {
            TagError::EmptyRequest => unreachable!("claim operation always changes one tag"),
            TagError::ProtectedReservation(_) => unreachable!("claim never removes a reservation"),
            TagError::ProtectedDecisionOptIn(_) => {
                unreachable!("claim never adds a decision opt-in")
            }
            TagError::MoleculeNotFound(_) => anyhow::anyhow!("failed to load molecule: {e}"),
            TagError::StoreUnavailable(_) => anyhow::anyhow!("failed to save molecule: {e}"),
        })?;

    if ctx.json {
        println!("{}", serde_json::to_string(&TagJson::from_delta(&delta))?);
    } else if claim {
        println!(
            "claimed {} — runtime will defer while hold:pilot is present",
            delta.id
        );
    } else {
        println!("released {} — runtime may dispatch it again", delta.id);
    }
    Ok(())
}
