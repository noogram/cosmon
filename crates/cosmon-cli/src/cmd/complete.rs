// SPDX-License-Identifier: AGPL-3.0-only

//! `cs complete` — shortcut to mark molecules as completed.
//!
//! Skips the full evolve ceremony: no formula needed, no step-by-step
//! validation. Just transitions the molecule directly to `Completed`.
//! Supports single and batch completion.
//!
//! # Mindguard `surface_visual`
//!
//! Every `cs complete <MOL>` runs the [`crate::mindguard::surface_visual`]
//! gate first. If `<MOL>` touched the visual surface (HTML/CSS/JS/wiki/
//! lumen-web) without a sibling `verify-surface` molecule landing
//! GREEN inside `T_max`, the transition refuses with `MindguardRefused`
//! and prints the `cs nucleate verify-surface --var target=<MOL>`
//! remedy. The gate is fail-closed — see
//! [`crate::mindguard`] for the axiome janis that mandates this.

use std::fs;

use chrono::Utc;
use colored::Colorize;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{event_log, StateStore};

use super::Context;
use crate::mindguard::{self, MindguardError};

/// Mark one or more molecules as completed.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to complete (single mode).
    #[arg(required_unless_present = "batch")]
    molecule: Option<String>,

    /// Complete multiple molecules at once.
    #[arg(long, num_args = 1..)]
    batch: Option<Vec<String>>,

    /// Reason for completion (recorded in the log).
    #[arg(long, default_value = "completed via cs complete")]
    reason: String,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<std::path::PathBuf>,

    /// Bypass mindguard *only when the gate machinery itself is
    /// unreachable*. Requires `--justification`. Lands a record in the
    /// append-only ledger at `~/.cosmon/audit/mindguard-overrides.jsonl`
    /// *before* the completion proceeds. NEVER use this to bypass a
    /// `MindguardRefused` — the remedy for that is to run
    /// `cs nucleate verify-surface --var target=<MOL>`.
    #[arg(long)]
    override_mindguard_down: bool,

    /// Justification for `--override-mindguard-down`. Required when the
    /// override flag is set. Recorded write-once in the audit ledger.
    #[arg(long)]
    justification: Option<String>,

    /// Skip the mindguard gate entirely (test/CI escape hatch). Hidden
    /// from `--help`. Used by integration tests that construct a
    /// `FileStore` in a non-git temp directory where the gate cannot
    /// run. Operator-facing flow must NEVER pass this — the override
    /// path with a justification is the only sanctioned bypass.
    #[arg(long, hide = true)]
    ignore_mindguard: bool,
}

/// Execute the `complete` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let ids = collect_ids(args)?;
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = FileStore::new(&ops_dir);

    if args.override_mindguard_down && args.justification.is_none() {
        anyhow::bail!(
            "--override-mindguard-down requires --justification \"…\" (recorded write-once in \
             ~/.cosmon/audit/mindguard-overrides.jsonl)"
        );
    }

    let mut results: Vec<serde_json::Value> = Vec::new();

    for mol_id in &ids {
        if !args.ignore_mindguard {
            if let Err(e) = run_mindguard(&store, mol_id, args, ctx) {
                if ctx.json {
                    results.push(serde_json::json!({
                        "molecule": mol_id.as_str(),
                        "error": format!("{e}"),
                        "mindguard": match &e {
                            MindguardError::Refused(_) => "refused",
                            MindguardError::Unavailable(_) => "unavailable",
                        },
                    }));
                }
                continue;
            }
        }
        match complete_one(&store, &ops_dir, mol_id, &args.reason) {
            Ok(prev_status) => {
                let already = prev_status == MoleculeStatus::Completed;
                if ctx.json {
                    results.push(serde_json::json!({
                        "molecule": mol_id.as_str(),
                        "previous_status": prev_status.to_string(),
                        "new_status": "completed",
                        "already_completed": already,
                        "reason": args.reason,
                    }));
                } else if already {
                    println!(
                        "{} {} already completed (no-op)",
                        MoleculeStatus::Completed.emoji(),
                        mol_id
                    );
                } else {
                    println!(
                        "{} {} completed (was {})",
                        MoleculeStatus::Completed.emoji(),
                        mol_id,
                        prev_status
                    );
                }
            }
            Err(e) => {
                if ctx.json {
                    results.push(serde_json::json!({
                        "molecule": mol_id.as_str(),
                        "error": format!("{e:#}"),
                    }));
                } else {
                    eprintln!("  error completing {mol_id}: {e:#}");
                }
            }
        }
    }

    if ctx.json {
        for r in &results {
            println!("{}", serde_json::to_string(r).unwrap_or_default());
        }
    }

    Ok(())
}

/// Collect molecule IDs from single or batch mode.
fn collect_ids(args: &Args) -> anyhow::Result<Vec<MoleculeId>> {
    let raw: Vec<&str> = if let Some(ref batch) = args.batch {
        batch.iter().map(String::as_str).collect()
    } else if let Some(ref single) = args.molecule {
        vec![single.as_str()]
    } else {
        anyhow::bail!("provide a molecule ID or use --batch");
    };

    raw.into_iter()
        .map(|s| MoleculeId::new(s).map_err(|e| anyhow::anyhow!("{e}")))
        .collect()
}

/// Complete a single molecule, returning the previous status on success.
///
/// Idempotent on `Completed`: if the molecule is already completed, this
/// returns `Ok(Completed)` without rewriting state. This matters because
/// the final `cs evolve` on the last formula step auto-completes the
/// molecule, so a subsequent `cs complete` (as emitted by the propulsion
/// prompt's terminal protocol) would otherwise fail spuriously.
///
/// Still errors on `Collapsed`: that is a deliberate terminal state and
/// overriding it to `Completed` would lose the collapse reason.
///
/// Exposed as `pub(crate)` so the in-process Direct-API branch of
/// `cs tackle` can call it as the canonical completion-emit site once
/// the agent loop returns Ok — see [`super::tackle::finalize_inprocess_molecule`]
/// and an internal chronicle for the
/// pattern divergence with tmux adapters (whose `pane-died` hook
/// indirectly drives this same transition via `cs harvest`).
pub(crate) fn complete_one(
    store: &FileStore,
    ops_dir: &std::path::Path,
    mol_id: &MoleculeId,
    reason: &str,
) -> anyhow::Result<MoleculeStatus> {
    // Hold the fleet lock for the load → check → save cycle (ADR-131
    // Decision 2: RAII guard — `_g` releases the flock at end of block).
    let prev_status = 'lock: {
        let _g = store.lock_fleet()?;
        let mol_data = store.load_molecule(mol_id)?;
        let prev_status = mol_data.status;

        if prev_status == MoleculeStatus::Completed {
            // Idempotent: already done.
            break 'lock prev_status;
        }
        if prev_status == MoleculeStatus::Collapsed {
            anyhow::bail!("molecule {mol_id} is collapsed — cannot complete a collapsed molecule");
        }

        // Transition to Completed.
        //
        // Set `current_step = total_steps` so observers report the molecule as
        // fully advanced (e.g. "2/2"). Without this, a completed molecule would
        // still display its pre-completion step index, contradicting its status.
        let mut updated = mol_data;
        updated.status = MoleculeStatus::Completed;
        updated.current_step = updated.total_steps;
        updated.updated_at = Utc::now();
        store.save_molecule(&updated.id.clone(), &updated)?;

        prev_status
    };

    // Append to log.md.
    let mol_dir = store.molecule_dir(mol_id);
    let log_path = mol_dir.join("log.md");
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let log_entry = format!("\n## {timestamp} — Completed\n\n{reason}\n");
    let existing_log = fs::read_to_string(&log_path).unwrap_or_default();
    let new_log = if existing_log.is_empty() {
        format!("# Evolution Log\n{log_entry}")
    } else {
        format!("{existing_log}{log_entry}")
    };
    fs::write(&log_path, new_log).map_err(|e| anyhow::anyhow!("failed to write log.md: {e}"))?;

    // Update briefing.md to reflect completion.
    let briefing_path = mol_dir.join("briefing.md");
    fs::write(
        &briefing_path,
        "# Molecule Briefing\n\n**Status:** COMPLETED\n\nCompleted via `cs complete`.\n",
    )
    .map_err(|e| anyhow::anyhow!("failed to write briefing.md: {e}"))?;

    // Seal proof-of-work manifest: capture artifact hashes so `cs verify`
    // can later detect tampering. Best-effort — a manifest write failure
    // does not abort the completion (the molecule is already transitioned).
    {
        let mol_data = store.load_molecule(mol_id).ok();
        let formula_id = mol_data
            .as_ref()
            .map(|m| m.formula_id.as_str().to_owned())
            .unwrap_or_default();
        let _ = crate::pow::seal(&mol_dir, mol_id.as_str(), &formula_id);
    }

    // Realized-model capture at the completion seam (delib-20260718-c70e /
    // F-01). The worker's session log is fully written by now, so this always-on
    // read records what actually ran — regardless of whether `cs peek` was ever
    // open. Best-effort and trace-not-lock; runs before `MoleculeCompleted` so a
    // fold sees the observation alongside the completion.
    crate::energy_probe::capture_realized_at_completion(ops_dir, mol_id);

    // Emit legacy events.
    let events_path = ops_dir.join("events.jsonl");
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeTransitioned {
            molecule_id: mol_id.clone(),
            from: prev_status,
            to: MoleculeStatus::Completed,
        }),
    );
    let _ = cosmon_filestore::event::append(
        &events_path,
        &Envelope::now(Event::MoleculeCompleted {
            molecule_id: mol_id.clone(),
            reason: reason.to_owned(),
        }),
    );

    // Emit EventV2 records.
    let status_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: prev_status.to_string(),
            to: "completed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeCompleted {
            molecule_id: mol_id.clone(),
            duration_ms: None,
            reason: reason.to_owned(),
        },
        status_seq,
    );

    Ok(prev_status)
}

/// Run the mindguard `surface_visual` gate for one molecule.
///
/// On [`MindguardError::Refused`] — prints the red remedy and returns
/// the error so the caller skips `complete_one`.
///
/// On [`MindguardError::Unavailable`] — if `--override-mindguard-down`
/// is set with a valid justification, lands a record in the ledger and
/// returns `Ok(())`. Otherwise prints the red error and returns it.
///
/// The fail-closed discipline is non-negotiable. A mindguard that
/// passes by default when it is down is a mindguard that agents learn
/// to make fall (janis §3d).
fn run_mindguard(
    store: &FileStore,
    mol_id: &MoleculeId,
    args: &Args,
    ctx: &Context,
) -> Result<(), MindguardError> {
    match mindguard::surface_visual::gate(store, mol_id) {
        Ok(()) => Ok(()),
        Err(MindguardError::Refused(msg)) => {
            if !ctx.json {
                eprintln!(
                    "{} {}",
                    "✘ mindguard surface_visual refused".red().bold(),
                    mol_id
                );
                eprintln!("  {msg}");
                eprintln!(
                    "  {}: this is NOT an override case — the gate fired \
                     intentionally. Land the missing verify-surface evidence.",
                    "note".yellow()
                );
            }
            Err(MindguardError::Refused(msg))
        }
        Err(MindguardError::Unavailable(msg)) => {
            if args.override_mindguard_down {
                // Caller already validated justification is_some.
                let justification = args
                    .justification
                    .as_deref()
                    .unwrap_or("(no justification recorded — bug)");
                match mindguard::ledger::append("surface_visual", mol_id, justification, &msg) {
                    Ok(_rec) => {
                        if !ctx.json {
                            eprintln!(
                                "{} {} (mindguard down: {msg})",
                                "⚠ mindguard override recorded".yellow().bold(),
                                mol_id
                            );
                        }
                        Ok(())
                    }
                    Err(e) => {
                        // Ledger write failed — refuse the override.
                        // A ledger we can write around silently is a
                        // ledger we cannot trust to audit later.
                        if !ctx.json {
                            eprintln!(
                                "{} {}: ledger write failed: {e}",
                                "✘ mindguard override refused".red().bold(),
                                mol_id
                            );
                        }
                        Err(MindguardError::Unavailable(format!(
                            "{msg}; AND ledger write failed: {e}"
                        )))
                    }
                }
            } else {
                if !ctx.json {
                    eprintln!(
                        "{} {}: {msg}",
                        "✘ mindguard unavailable".red().bold(),
                        mol_id
                    );
                    eprintln!(
                        "  remedy: cs complete --override-mindguard-down \
                         --justification \"…\" {mol_id}"
                    );
                }
                Err(MindguardError::Unavailable(msg))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        (tmp, store)
    }

    fn sample_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
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
            adapter: None,
        }
    }

    #[test]
    fn test_complete_sets_current_step_to_total() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-0001", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let result = complete_one(&store, tmp.path(), &mol.id, "test").unwrap();
        assert_eq!(result, MoleculeStatus::Running);

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Completed);
        assert_eq!(reloaded.current_step, reloaded.total_steps);
        assert_eq!(reloaded.current_step, 2);
    }

    #[test]
    fn test_complete_is_idempotent_on_completed() {
        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260409-0002", MoleculeStatus::Completed);
        mol.current_step = mol.total_steps;
        store.save_molecule(&mol.id, &mol).unwrap();
        let original_updated_at = mol.updated_at;

        // Second call should succeed silently, returning Completed as prev_status.
        let result = complete_one(&store, tmp.path(), &mol.id, "retry").unwrap();
        assert_eq!(result, MoleculeStatus::Completed);

        // Verify state was NOT rewritten (updated_at unchanged).
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.updated_at, original_updated_at);
    }

    #[test]
    fn test_complete_rejects_collapsed() {
        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260409-0003", MoleculeStatus::Collapsed);
        mol.collapse_reason = Some("test collapse".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let err = complete_one(&store, tmp.path(), &mol.id, "nope").unwrap_err();
        assert!(err.to_string().contains("collapsed"));
    }

    #[test]
    fn test_complete_from_pending() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-0004", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        complete_one(&store, tmp.path(), &mol.id, "skip").unwrap();
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Completed);
        assert_eq!(reloaded.current_step, reloaded.total_steps);
    }
}
