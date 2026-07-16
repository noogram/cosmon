// SPDX-License-Identifier: AGPL-3.0-only

//! `cs heartbeat` — emit a periodic liveness signal for the calling worker.
//!
//! Workers call this every 30–60 seconds (from their harness or bridge) so the
//! runtime can distinguish a "thinking" worker from a "stuck" worker without
//! introspecting tmux. The event lands in `events.jsonl` as a
//! [`EventV2::WorkerHeartbeat`] record.

use cosmon_core::event_v2::{ActivityHint, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_filestore::FileStore;
use cosmon_state::StateStore;

use super::Context;

/// Arguments for the `heartbeat` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// The worker emitting the heartbeat.
    #[arg(long, value_name = "WORKER_ID")]
    worker: String,

    /// Activity classification: `thinking`, `waiting_io`, `idle`, `unknown`.
    #[arg(long, value_name = "HINT", default_value = "unknown")]
    activity: String,

    /// Optional molecule whose `last_progress_at` should be bumped to "now"
    /// alongside the heartbeat event. This is a liveness/progress signal only;
    /// it never advances `last_output_at`, which records durable output.
    #[arg(long, value_name = "MOLECULE_ID")]
    molecule: Option<String>,
}

/// Emit a single [`EventV2::WorkerHeartbeat`] and exit.
///
/// # Errors
///
/// Returns an error if the worker ID is invalid, the activity hint is
/// unrecognised, or the event log cannot be written.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let worker_id = WorkerId::new(&args.worker)
        .map_err(|e| anyhow::anyhow!("invalid worker id '{}': {e}", args.worker))?;

    let activity_hint = match args.activity.as_str() {
        "thinking" => ActivityHint::Thinking,
        "waiting_io" | "waiting-io" => ActivityHint::WaitingIo,
        "idle" => ActivityHint::Idle,
        "unknown" => ActivityHint::Unknown,
        other => anyhow::bail!(
            "unknown activity hint '{other}' (expected thinking|waiting_io|idle|unknown)"
        ),
    };

    let events_path = super::default_state_dir().join("events.jsonl");
    let seq = cosmon_state::event_log::emit_one(
        &events_path,
        EventV2::WorkerHeartbeat {
            worker_id: worker_id.clone(),
            ts: chrono::Utc::now(),
            activity_hint,
        },
        None,
    )?;

    let mut bumped_molecule: Option<MoleculeId> = None;
    if let Some(raw) = args.molecule.as_deref() {
        let mol_id = MoleculeId::new(raw)
            .map_err(|e| anyhow::anyhow!("invalid molecule id '{raw}': {e}"))?;
        let store = FileStore::new(super::default_state_dir());
        {
            // ADR-131 Decision 2: RAII guard replaces the lock-bounding closure.
            let _g = store.lock_fleet()?;
            let mut mol = store.load_molecule(&mol_id)?;
            let now = chrono::Utc::now();
            mol.last_progress_at = Some(now);
            mol.updated_at = now;
            store.save_molecule(&mol_id, &mol)?;
        }
        bumped_molecule = Some(mol_id);
    }

    if ctx.json {
        let out = serde_json::json!({
            "worker": worker_id.as_str(),
            "activity": args.activity,
            "seq": seq.0,
            "molecule": bumped_molecule.as_ref().map(MoleculeId::as_str),
        });
        println!("{out}");
    } else if let Some(ref mol) = bumped_molecule {
        println!("💓 heartbeat {worker_id} ({}) → {mol}", args.activity);
    } else {
        println!("💓 heartbeat {worker_id} ({})", args.activity);
    }
    Ok(())
}
