// SPDX-License-Identifier: AGPL-3.0-only

//! `cs decay`, `cs merge`, `cs transform` — molecule interactions.
//!
//! Inter-molecule operations that create, consume, or transform molecules.
//! Each interaction is explicit (triggered by agent/operator, never automatic)
//! and subject to the Attention Conservation Law (THESIS Part XVII).

use chrono::Utc;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::{Interaction, MoleculeLink};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::MoleculeData;

use super::Context;

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
#[allow(clippy::too_many_lines)]
pub fn run_decay(ctx: &Context, args: &DecayArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let source_id = MoleculeId::new(&args.source)?;

    // f4e1 guard: cs decay copies source.variables verbatim to every
    // product, so --count > 1 creates N byte-identical children. Refuse
    // before touching state; the operator must use N separate
    // `cs nucleate ... --blocks <source>` invocations instead.
    super::guard::ensure_decay_count_is_heterogeneous(&source_id, args.count)
        .map_err(anyhow::Error::from)?;

    let mut source = store.load_molecule(&source_id)?;

    let source_kind = source.kind.unwrap_or(MoleculeKind::Task);
    if !source_kind.can_decay() {
        return Err(anyhow::anyhow!(
            "molecule kind '{source_kind}' cannot decay (only idea and issue can)"
        ));
    }

    let product_kind: MoleculeKind = args
        .product_kind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid product kind: {e}"))?;

    // Load formula for products.
    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", args.formula));
    let toml_text = std::fs::read_to_string(&formula_path)
        .map_err(|e| anyhow::anyhow!("formula not found: {e}"))?;
    let formula = cosmon_core::formula::Formula::parse(&toml_text)?;

    // Nucleate products.
    let mut product_ids = Vec::new();
    let mut rng = rand::thread_rng();
    for _ in 0..args.count {
        let nuc = cosmon_core::nucleate::nucleate(
            cosmon_core::nucleate::NucleateRequest {
                formula: &formula,
                variables: source.variables.clone(),
                assign: source.assigned_worker.clone(),
            },
            &mut rng,
        )?;

        let product = MoleculeData {
            id: nuc.id.clone(),
            fleet_id: source.fleet_id.clone(),
            formula_id: nuc.formula_id.clone(),
            status: nuc.status,
            variables: nuc.variables.clone(),
            assigned_worker: nuc.assigned_worker.clone(),
            created_at: nuc.created_at,
            updated_at: nuc.created_at,
            total_steps: nuc.total_steps,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: Some(product_kind),
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![MoleculeLink::DecayedFrom {
                id: source_id.clone(),
            }],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        };
        store.save_molecule(&nuc.id, &product)?;
        product_ids.push(nuc.id);
    }

    // Wire --chain: product[i] blocks product[i+1].
    if args.chain && product_ids.len() > 1 {
        for pair in product_ids.windows(2) {
            let upstream = &pair[0];
            let downstream = &pair[1];
            let mut u = store.load_molecule(upstream)?;
            u.typed_links.push(MoleculeLink::Blocks {
                target: downstream.clone(),
            });
            store.save_molecule(upstream, &u)?;
            let mut d = store.load_molecule(downstream)?;
            d.typed_links.push(MoleculeLink::BlockedBy {
                source: upstream.clone(),
            });
            store.save_molecule(downstream, &d)?;
        }
    }

    // Wire --blocks: each product[i] blocks the specified molecule.
    for (i, target_str) in args.blocks.iter().enumerate() {
        let target_id = MoleculeId::new(target_str)?;
        let prod_idx = i.min(product_ids.len() - 1);
        let prod_id = &product_ids[prod_idx];
        let mut prod = store.load_molecule(prod_id)?;
        prod.typed_links.push(MoleculeLink::Blocks {
            target: target_id.clone(),
        });
        store.save_molecule(prod_id, &prod)?;
        let mut target_mol = store.load_molecule(&target_id)?;
        target_mol.typed_links.push(MoleculeLink::BlockedBy {
            source: prod_id.clone(),
        });
        store.save_molecule(&target_id, &target_mol)?;
    }

    // Complete the source.
    source.status = MoleculeStatus::Completed;
    source.updated_at = Utc::now();
    for pid in &product_ids {
        source
            .typed_links
            .push(MoleculeLink::DecayProduct { id: pid.clone() });
    }
    store.save_molecule(&source_id, &source)?;

    // Log interaction.
    let interaction = Interaction::Decay {
        source: source_id.clone(),
        products: product_ids.clone(),
        reason: args.reason.clone(),
        timestamp: Utc::now(),
    };
    log_interaction(&state_dir, &interaction)?;

    // Emit legacy molecule_decayed event.
    let _ = cosmon_filestore::event::append(
        &state_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeDecayed {
            molecule_id: source_id.clone(),
            products: product_ids.clone(),
            reason: args.reason.clone(),
        }),
    );

    // Emit EventV2::DecaySpliced.
    let _ = cosmon_state::event_log::emit_one(
        state_dir.join("events.jsonl"),
        cosmon_core::event_v2::EventV2::DecaySpliced {
            parent: source_id.clone(),
            children: product_ids.clone(),
        },
        None,
    );

    if ctx.json {
        let output = serde_json::json!({
            "interaction": "decay",
            "source": source_id.as_str(),
            "products": product_ids.iter().map(cosmon_core::id::MoleculeId::as_str).collect::<Vec<_>>(),
            "product_kind": product_kind.to_string(),
            "reason": args.reason,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Decayed {} → {} products:", source_id, product_ids.len());
        for pid in &product_ids {
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
#[allow(clippy::too_many_lines)]
pub fn run_merge(ctx: &Context, args: &MergeArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    if args.sources.len() < 2 {
        return Err(anyhow::anyhow!(
            "merge requires at least 2 source molecules"
        ));
    }

    let source_ids: Vec<MoleculeId> = args
        .sources
        .iter()
        .map(|s| MoleculeId::new(s).map_err(|e| anyhow::anyhow!("invalid id: {e}")))
        .collect::<Result<_, _>>()?;

    // Validate all sources can merge.
    let mut sources: Vec<MoleculeData> = Vec::new();
    for sid in &source_ids {
        let mol = store.load_molecule(sid)?;
        let kind = mol.kind.unwrap_or(MoleculeKind::Task);
        if !kind.can_merge() {
            return Err(anyhow::anyhow!(
                "molecule {sid} (kind: {kind}) cannot participate in merge"
            ));
        }
        sources.push(mol);
    }

    let product_kind: MoleculeKind = args
        .product_kind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid product kind: {e}"))?;

    // Use the first source's fleet and variables as base.
    let fleet_id = sources[0].fleet_id.clone();

    // Merge variables from all sources.
    let mut merged_vars = std::collections::HashMap::new();
    for src in &sources {
        merged_vars.extend(src.variables.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    // Load formula and nucleate product.
    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", args.formula));
    let toml_text = std::fs::read_to_string(&formula_path)
        .map_err(|e| anyhow::anyhow!("formula not found: {e}"))?;
    let formula = cosmon_core::formula::Formula::parse(&toml_text)?;

    let nuc = cosmon_core::nucleate::nucleate(
        cosmon_core::nucleate::NucleateRequest {
            formula: &formula,
            variables: merged_vars,
            assign: sources[0].assigned_worker.clone(),
        },
        &mut rand::thread_rng(),
    )?;

    let product = MoleculeData {
        id: nuc.id.clone(),
        fleet_id,
        formula_id: nuc.formula_id.clone(),
        status: nuc.status,
        variables: nuc.variables.clone(),
        assigned_worker: nuc.assigned_worker.clone(),
        created_at: nuc.created_at,
        updated_at: nuc.created_at,
        total_steps: nuc.total_steps,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: Some(product_kind),
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: vec![MoleculeLink::MergedFrom {
            ids: source_ids.clone(),
        }],
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    };
    store.save_molecule(&nuc.id, &product)?;

    // Complete all sources.
    for (sid, mut src) in source_ids.iter().zip(sources) {
        src.status = MoleculeStatus::Completed;
        src.updated_at = Utc::now();
        src.typed_links
            .push(MoleculeLink::MergedInto { id: nuc.id.clone() });
        store.save_molecule(sid, &src)?;
    }

    // Log interaction.
    let interaction = Interaction::Merge {
        sources: source_ids.clone(),
        product: nuc.id.clone(),
        reason: args.reason.clone(),
        timestamp: Utc::now(),
    };
    log_interaction(&state_dir, &interaction)?;

    // Emit molecule_merged event.
    let _ = cosmon_filestore::event::append(
        &state_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeMerged {
            sources: source_ids.clone(),
            product: nuc.id.clone(),
            reason: args.reason.clone(),
        }),
    );

    if ctx.json {
        let output = serde_json::json!({
            "interaction": "merge",
            "sources": source_ids.iter().map(cosmon_core::id::MoleculeId::as_str).collect::<Vec<_>>(),
            "product": nuc.id.as_str(),
            "product_kind": product_kind.to_string(),
            "reason": args.reason,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Merged {} sources → {}:", source_ids.len(), nuc.id);
        for sid in &source_ids {
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
    let mut mol = store.load_molecule(&mol_id)?;

    let from_kind = mol.kind.unwrap_or(MoleculeKind::Task);
    let to_kind: MoleculeKind = args
        .to
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid target kind: {e}"))?;

    if !from_kind.can_transform_to(to_kind) {
        return Err(anyhow::anyhow!(
            "cannot transform {from_kind} → {to_kind} (valid targets: {:?})",
            from_kind.valid_transforms()
        ));
    }

    mol.kind = Some(to_kind);
    mol.updated_at = Utc::now();
    mol.typed_links
        .push(MoleculeLink::TransformedFrom { kind: from_kind });
    store.save_molecule(&mol_id, &mol)?;

    // Log interaction.
    let interaction = Interaction::Transform {
        molecule: mol_id.clone(),
        from: from_kind,
        to: to_kind,
        reason: args.reason.clone(),
        timestamp: Utc::now(),
    };
    log_interaction(&state_dir, &interaction)?;

    // Emit molecule_transformed event.
    let _ = cosmon_filestore::event::append(
        &state_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeTransformed {
            molecule_id: mol_id.clone(),
            from_kind,
            to_kind,
            reason: args.reason.clone(),
        }),
    );

    if ctx.json {
        let output = serde_json::json!({
            "interaction": "transform",
            "molecule": mol_id.as_str(),
            "from": from_kind.to_string(),
            "to": to_kind.to_string(),
            "reason": args.reason,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Transformed {mol_id} : {from_kind} → {to_kind}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Interaction logging
// ---------------------------------------------------------------------------

/// Append an interaction to the interaction log (`.cosmon/interactions.jsonl`).
fn log_interaction(state_dir: &std::path::Path, interaction: &Interaction) -> anyhow::Result<()> {
    use std::io::Write;

    let log_path = state_dir.join("interactions.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let json = serde_json::to_string(interaction)?;
    writeln!(file, "{json}")?;
    Ok(())
}
