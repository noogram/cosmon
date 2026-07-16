// SPDX-License-Identifier: AGPL-3.0-only

//! `cs await-operator <id> --question <q>...` — the **only** sanctioned
//! way for a worker to block on an operator decision (ADR-123).
//!
//! A worker that reaches an *undecidable-AND-irreversible* boundary
//! (signature transmitted, push to a shared remote, publish, an
//! authoritative value downstream consumers act on) calls this verb
//! instead of raising an off-cosmon modal. The verb routes on the
//! molecule's typed capability (the `op-block:<boundary>` tag granted at
//! `cs nucleate`):
//!
//! - **capability present** ⇒ **block**: write `blocked_on.json` to the
//!   molecule dir, emit [`EventV2::WorkerBlockedOnOperator`], stamp
//!   `temp:awaiting-op` + the boundary's alert tag, and tell the worker
//!   to **yield**. The molecule stays `Running`.
//! - **capability absent** ⇒ **surface-and-continue**: write the options
//!   and a recommended default to `responses/`, and tell the worker to
//!   **keep working** — never block.
//!
//! > **`AskUserQuestion`-in-tmux is forbidden as a blocking primitive**
//! > (ADR-123 Q5). It lives in Claude Code, external to cosmon's state
//! > machine, so it can never satisfy the *emitted-not-inferred*
//! > requirement: a block raised through it is invisible and the
//! > DAG stalls silently. The cosmon-native
//! > harness does not register any modal tool by construction
//! > (`cosmon_agent_harness::default_registry`), and this verb is the
//! > single emit path. The un-emitting case (a worker that blocks anyway,
//! > off-cosmon) is caught by the external event-age patrol
//! > (`cs patrol --event-age`).

use std::path::Path;

use chrono::{DateTime, Utc};
use cosmon_core::auth::Subject;
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_block::IrreversibleBoundary;
use cosmon_state::ops::{
    await_operator, AwaitOperatorJson, AwaitOperatorOutcome, AwaitOperatorRequest,
};
use serde::Serialize;

use super::Context;

/// Arguments for the `await-operator` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID whose worker is blocking.
    pub molecule_id: String,

    /// A decision the operator is being asked to make (repeatable).
    /// At least one is required.
    #[arg(long = "question", value_name = "TEXT", required = true)]
    pub questions: Vec<String>,
}

/// Persisted `blocked_on.json` record (schema `blocked_on/v1`).
///
/// The durable, machine-readable proof-of-block written to the molecule
/// dir at the moment of the pause. Pairs with the
/// [`EventV2::WorkerBlockedOnOperator`](cosmon_core::event_v2::EventV2)
/// ledger entry: the event is the authoritative signal, this file is the
/// human-readable payload (the questions) the operator answers.
#[derive(Debug, Serialize)]
struct BlockedOnRecord {
    schema: &'static str,
    molecule_id: String,
    boundary: String,
    since: DateTime<Utc>,
    questions: Vec<String>,
}

/// Execute the `await-operator` command.
///
/// # Errors
/// Fails if the molecule does not exist, is terminal, the request carries
/// no question, or persistence fails.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let mol_id =
        MoleculeId::new(&args.molecule_id).map_err(|e| anyhow::anyhow!("invalid id: {e}"))?;

    let view = await_operator(
        store.as_ref(),
        &state_dir,
        &Subject::operator(),
        &mol_id,
        AwaitOperatorRequest {
            questions: args.questions.clone(),
        },
    )
    .map_err(|e| anyhow::anyhow!("await-operator failed: {e}"))?;

    let mol_dir = cosmon_state::archive::resolve_molecule_dir(&state_dir, &mol_id);

    match &view.outcome {
        AwaitOperatorOutcome::Blocked { boundary, since } => {
            if let Some(dir) = &mol_dir {
                write_blocked_on(dir, &mol_id, *boundary, *since, &view.questions)?;
            }
            render_blocked(ctx, &view, *boundary, mol_dir.as_deref());
        }
        AwaitOperatorOutcome::SurfaceAndContinue => {
            let surface = mol_dir
                .as_deref()
                .map(|dir| write_needs_review(dir, &mol_id, &view.questions))
                .transpose()?
                .flatten();
            render_surfaced(ctx, &view, surface.as_deref());
        }
    }

    Ok(())
}

/// Write the `blocked_on.json` proof-of-block to the molecule dir.
fn write_blocked_on(
    mol_dir: &Path,
    mol_id: &MoleculeId,
    boundary: IrreversibleBoundary,
    since: DateTime<Utc>,
    questions: &[String],
) -> anyhow::Result<()> {
    let record = BlockedOnRecord {
        schema: "blocked_on/v1",
        molecule_id: mol_id.to_string(),
        boundary: boundary.to_string(),
        since,
        questions: questions.to_vec(),
    };
    let body = serde_json::to_string_pretty(&record)?;
    std::fs::write(mol_dir.join("blocked_on.json"), body)?;
    Ok(())
}

/// Write the surfaced proposal to `responses/` and return its path.
///
/// The surface-and-continue contract (architect's default regime): the
/// worker records the options + a recommended default so the operator can
/// review *after the fact*, then keeps working. Never blocks.
fn write_needs_review(
    mol_dir: &Path,
    mol_id: &MoleculeId,
    questions: &[String],
) -> anyhow::Result<Option<std::path::PathBuf>> {
    use std::fmt::Write as _;

    let responses = mol_dir.join("responses");
    std::fs::create_dir_all(&responses)?;
    let path = responses.join("needs-review.md");
    let mut body = String::new();
    let _ = writeln!(body, "# Needs review — {mol_id}\n");
    body.push_str(
        "This molecule has **no `op-block:*` capability**, so the worker \
         surfaced these decisions and **continued** rather than blocking \
         (ADR-123 surface-and-continue default). The action was reversible \
         (`git` + `cs` can undo it before `cs done`); review at leisure.\n\n",
    );
    body.push_str("## Decisions surfaced\n\n");
    for q in questions {
        let _ = writeln!(body, "- {q}");
    }
    body.push_str(
        "\n> If this molecule SHOULD be allowed to block (the next action \
         is genuinely irreversible — a signature, a push to a shared remote, \
         a publish), re-nucleate it with \
         `cs nucleate ... --may-block-on-operator <boundary>` so the worker \
         reads the capability and pauses observably instead of guessing.\n",
    );
    std::fs::write(&path, body)?;
    Ok(Some(path))
}

fn render_blocked(
    ctx: &Context,
    view: &cosmon_state::ops::AwaitOperatorView,
    boundary: IrreversibleBoundary,
    mol_dir: Option<&Path>,
) {
    if ctx.json {
        if let Ok(line) = serde_json::to_string(&AwaitOperatorJson::from_view(view)) {
            println!("{line}");
        }
        return;
    }
    println!(
        "🛑 BLOCKED on operator — {} (boundary: {boundary})",
        view.id
    );
    println!("   emitted worker_blocked_on_operator + tagged temp:awaiting-op");
    if let Some(dir) = mol_dir {
        println!("   wrote {}", dir.join("blocked_on.json").display());
    }
    println!(
        "   YIELD now — do not act past the irreversible boundary until the operator answers:"
    );
    for q in &view.questions {
        println!("     • {q}");
    }
}

fn render_surfaced(
    ctx: &Context,
    view: &cosmon_state::ops::AwaitOperatorView,
    surface: Option<&Path>,
) {
    if ctx.json {
        if let Ok(line) = serde_json::to_string(&AwaitOperatorJson::from_view(view)) {
            println!("{line}");
        }
        return;
    }
    println!(
        "↪️  SURFACE-AND-CONTINUE — {} has no op-block capability",
        view.id
    );
    if let Some(path) = surface {
        println!("   wrote {}", path.display());
    }
    println!("   CONTINUE working — pick a sensible default and act; this is reversible.");
    for q in &view.questions {
        println!("     • {q}");
    }
}
