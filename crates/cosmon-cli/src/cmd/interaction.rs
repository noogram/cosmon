// SPDX-License-Identifier: AGPL-3.0-only

//! `cs decay`, `cs merge`, `cs transform` — molecule interactions.
//!
//! Inter-molecule operations that create, consume, or transform molecules.
//! Each interaction is explicit (triggered by agent/operator, never automatic)
//! and subject to the Attention Conservation Law (THESIS Part XVII).
//!
//! The work is [`cosmon_state::ops::decay`], [`cosmon_state::ops::merge`] and
//! [`cosmon_state::ops::transform`], which `POST /v1/molecules/{id}/decay`,
//! `/merge` and `/transform` call too. This module parses arguments, picks the
//! directories and renders the result.

use cosmon_core::id::MoleculeId;
use cosmon_state::ops;

use super::Context;

/// The legacy event writer both `cs` and the RPP hand to the shared
/// operations.
struct LegacyEvents;

impl ops::InteractionEffects for LegacyEvents {
    fn append_legacy_event(
        &self,
        events_path: &std::path::Path,
        envelope: &cosmon_core::event::Envelope,
    ) {
        let _ = cosmon_filestore::event::append(events_path, envelope);
    }
}

fn parse_ids(raw: &[String]) -> anyhow::Result<Vec<MoleculeId>> {
    raw.iter()
        .map(|s| MoleculeId::new(s).map_err(|e| anyhow::anyhow!("invalid id: {e}")))
        .collect()
}

// ---------------------------------------------------------------------------
// cs decay — 1 molecule → N child molecules
// ---------------------------------------------------------------------------

/// Arguments for the `decay` subcommand.
#[derive(clap::Args)]
pub struct DecayArgs {
    /// Source molecule ID to decay.
    source: String,
    /// Formula for the product molecules.
    #[arg(long)]
    formula: String,
    /// Number of products to create (or provide --product-vars multiple times).
    #[arg(long, default_value = "1")]
    count: usize,
    /// Kind for the product molecules (default: task).
    #[arg(long, default_value = "task")]
    product_kind: String,
    /// Reason for the decay.
    #[arg(long)]
    reason: String,
    /// Wire consecutive decay products with Blocks/BlockedBy links (A→B→C).
    #[arg(long)]
    chain: bool,
    /// Explicit Blocks edges: the i-th product blocks the given molecule IDs.
    /// Repeatable; applied to each product in order.
    #[arg(long)]
    blocks: Vec<String>,
}

/// Execute the `decay` command.
pub fn run_decay(ctx: &Context, args: &DecayArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);
    let source_id = MoleculeId::new(&args.source)?;

    // f4e1 guard, before anything is read: it keeps its own exit code.
    super::guard::ensure_decay_count_is_heterogeneous(&source_id, args.count)
        .map_err(anyhow::Error::from)?;

    let blocks = args
        .blocks
        .iter()
        .map(MoleculeId::new)
        .collect::<Result<Vec<_>, _>>()?;
    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let view = ops::decay(
        &*store,
        &state_dir,
        &formulas_dir,
        &ops::DecayRequest {
            source: &source_id,
            formula: &args.formula,
            count: args.count,
            product_kind: &args.product_kind,
            reason: &args.reason,
            chain: args.chain,
            blocks: &blocks,
        },
        &LegacyEvents,
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ops::DecayJson::from_view(&view))?
        );
    } else {
        println!(
            "Decayed {} → {} products:",
            view.source,
            view.products.len()
        );
        for pid in &view.products {
            println!("  {pid}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cs merge — N molecules → 1 molecule
// ---------------------------------------------------------------------------

/// Arguments for the `merge` subcommand.
#[derive(clap::Args)]
pub struct MergeArgs {
    /// Source molecule IDs to merge (space-separated).
    sources: Vec<String>,
    /// Formula for the product molecule.
    #[arg(long)]
    formula: String,
    /// Kind for the product molecule (default: decision).
    #[arg(long, default_value = "decision")]
    product_kind: String,
    /// Reason for the merge.
    #[arg(long)]
    reason: String,
}

/// Execute the `merge` command.
pub fn run_merge(ctx: &Context, args: &MergeArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);
    let source_ids = parse_ids(&args.sources)?;
    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let view = ops::merge(
        &*store,
        &state_dir,
        &formulas_dir,
        &ops::MergeRequest {
            sources: &source_ids,
            formula: &args.formula,
            product_kind: &args.product_kind,
            reason: &args.reason,
        },
        &LegacyEvents,
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ops::MergeJson::from_view(&view))?
        );
    } else {
        println!("Merged {} sources → {}:", view.sources.len(), view.product);
        for sid in &view.sources {
            println!("  {sid} (completed)");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cs transform — change a molecule's kind
// ---------------------------------------------------------------------------

/// Arguments for the `transform` subcommand.
#[derive(clap::Args)]
pub struct TransformArgs {
    /// Molecule ID to transform.
    molecule: String,
    /// Target kind: idea, task, decision, issue.
    #[arg(long)]
    to: String,
    /// Reason for the transform.
    #[arg(long)]
    reason: String,
}

/// Execute the `transform` command.
pub fn run_transform(ctx: &Context, args: &TransformArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);
    let mol_id = MoleculeId::new(&args.molecule)?;
    let view = ops::transform(
        &*store,
        &state_dir,
        &ops::TransformRequest {
            molecule: &mol_id,
            to: &args.to,
            reason: &args.reason,
        },
        &LegacyEvents,
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ops::TransformJson::from_view(&view))?
        );
    } else {
        println!(
            "Transformed {} : {} → {}",
            view.molecule, view.from, view.to
        );
    }
    Ok(())
}
