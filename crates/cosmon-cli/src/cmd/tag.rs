// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tag <id>` — add or remove typed labels on a molecule.
//!
//! Idempotent: adding a tag that is already present is a no-op, and
//! removing a tag that is not present is a no-op.
//!
//! Library-first promotion: the CLI is a thin wrapper over
//! [`cosmon_state::ops::tag`](fn@cosmon_state::ops::tag). Both `cs tag` and the cs-api
//! `POST /molecules/{id}/tag` route share the same in-process code path,
//! so behaviour stays identical across surfaces and there is a single
//! call site to audit for the single-writer hazard.

use cosmon_core::id::MoleculeId;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::ops::{tag, TagError, TagJson};

use super::Context;

/// Arguments for the `tag` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to retag.
    pub molecule_id: String,

    /// Tag to add (repeatable). Format: `key` or `key:value`.
    #[arg(long = "add", value_name = "TAG")]
    pub add: Vec<String>,

    /// Tag to remove (repeatable). Matched by exact string.
    #[arg(long = "remove", value_name = "TAG")]
    pub remove: Vec<String>,
}

/// Execute the `tag` command.
///
/// # Errors
/// Fails if the molecule does not exist, any tag is invalid, the request
/// is empty, or persistence fails.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let mol_id =
        MoleculeId::new(&args.molecule_id).map_err(|e| anyhow::anyhow!("invalid id: {e}"))?;

    let add: Vec<Tag> = args
        .add
        .iter()
        .map(|s| Tag::new(s.clone()).map_err(|e| anyhow::anyhow!("invalid --add `{s}`: {e}")))
        .collect::<Result<_, _>>()?;
    let remove: Vec<Tag> = args
        .remove
        .iter()
        .map(|s| Tag::new(s.clone()).map_err(|e| anyhow::anyhow!("invalid --remove `{s}`: {e}")))
        .collect::<Result<_, _>>()?;

    // T-AUTHZ-INSTR — `subject_kind = "operator"` is the V0 placeholder
    // for the trusted CLI subject; T-SUBJECT will replace this with a
    // typed `Subject` once it lands.
    let delta =
        tag(&store, &state_dir, "operator", &mol_id, &add, &remove).map_err(|e| match e {
            TagError::EmptyRequest => {
                anyhow::anyhow!("nothing to do — supply --add and/or --remove")
            }
            TagError::ProtectedReservation(tag) => anyhow::anyhow!(
                "cannot remove protected runtime reservation `{tag}`; it is monotone until terminal teardown"
            ),
            TagError::ProtectedDecisionOptIn(tag) => anyhow::anyhow!(
                "cannot add protected runtime decision opt-in `{tag}` through the worker-reachable tag surface"
            ),
            TagError::MoleculeNotFound(_) => anyhow::anyhow!("failed to load molecule: {e}"),
            TagError::StoreUnavailable(_) => anyhow::anyhow!("failed to save molecule: {e}"),
        })?;

    if ctx.json {
        let out = TagJson::from_delta(&delta);
        let line = serde_json::to_string(&out)?;
        println!("{line}");
    } else {
        println!(
            "tagged {} → {} tag{} ({} added, {} removed)",
            delta.id,
            delta.after_count,
            if delta.after_count == 1 { "" } else { "s" },
            delta.requested_add.len(),
            delta.requested_remove.len(),
        );
        for t in &delta.final_tags {
            println!("  {t}");
        }
    }
    Ok(())
}
