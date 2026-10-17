// SPDX-License-Identifier: AGPL-3.0-only

//! `cs stuck` — freeze a molecule and record the blocker.
//!
//! When a worker cannot proceed, it calls `cs stuck` to freeze
//! the molecule with a reason. The molecule can be thawed later.

use super::Context;

/// Arguments for the `stuck` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID that is stuck.
    molecule: String,
    /// What is blocking progress.
    #[arg(long)]
    reason: String,
}

/// Execute the `stuck` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let mol_id = cosmon_core::id::MoleculeId::new(&args.molecule)?;
    cosmon_state::ops::stuck::stuck(
        store.as_ref(),
        &state_dir,
        &cosmon_core::auth::Subject::operator(),
        &mol_id,
        cosmon_state::ops::stuck::StuckRequest {
            reason: args.reason.clone(),
        },
    )?;
    let mut mol = store.load_molecule(&mol_id)?;

    // ADR-030 M3 — archive the stuck molecule's state so a reclone or
    // triage review can see the blocker without running `cs`. Non-fatal:
    // any failure warns and the `stuck` transition still succeeds. The
    // idempotence gate (`mol.archived`) ensures re-running `cs stuck`
    // with the same reason is a no-op on the archive.
    let config_path = super::resolve_config_from_context(ctx);
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();
    if project_config.archive.enabled && !mol.archived {
        let archive_mol_dir = cosmon_state::archive::resolve_molecule_dir(&state_dir, &mol_id)
            .unwrap_or_else(|| store.molecule_dir(&mol_id));
        if cosmon_state::archive::write_non_fatal(
            &state_dir,
            &archive_mol_dir,
            &mol,
            cosmon_state::archive::Trigger::Stuck,
            chrono::Utc::now(),
        )
        .is_some()
        {
            mol.archived = true;
            let _ = store.save_molecule(&mol_id, &mol);
        }
    }

    if ctx.json {
        let output = serde_json::json!({
            "status": "stuck",
            "molecule": mol_id.as_str(),
            "reason": args.reason,
            "archived": mol.archived,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("❄️ {} stuck: {}", mol_id, args.reason);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};

    use super::{run, Args, Context};

    fn enable_archive_config(state_dir: &std::path::Path) {
        let cosmon_dir = state_dir.parent().unwrap();
        std::fs::write(
            cosmon_dir.join("config.toml"),
            "[project]\nproject_id = \"test-stuck\"\n\n[archive]\nenabled = true\n",
        )
        .unwrap();
    }

    fn mol(id: &str) -> MoleculeData {
        let now = chrono::Utc::now();
        MoleculeData {
            harvest_reason: None,
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 2,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: vec![],
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            base_branch: None,
            protected_paths: Vec::new(),
            pending_step: None,
            merged_at: None,
            non_integration: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            collapse_receipt: None,
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
    fn stuck_archives_and_sets_archived_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let m = mol("task-20260415-stk1");
        store.save_molecule(&m.id, &m).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-stk1".to_owned(),
            reason: "upstream pending".to_owned(),
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(
            reloaded.archived,
            "archived flag should be true after cs stuck"
        );
        assert_eq!(reloaded.status, MoleculeStatus::Frozen);

        assert!(state_dir.join("archive").is_dir());
    }

    #[test]
    fn stuck_is_idempotent_on_archived_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        enable_archive_config(&state_dir);

        let store = FileStore::new(&state_dir);
        let mut m = mol("task-20260415-stk2");
        m.archived = true;
        store.save_molecule(&m.id, &m).unwrap();

        let archive_root = state_dir.join("archive");
        assert!(!archive_root.exists());

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir.clone()),
        };
        let args = Args {
            molecule: "task-20260415-stk2".to_owned(),
            reason: "replay blocker".to_owned(),
        };
        run(&ctx, &args).unwrap();

        assert!(
            !archive_root.exists(),
            "archived flag must short-circuit the write"
        );
    }
    #[test]
    fn stuck_refuses_terminal_without_state_or_events_changes() {
        for status in [MoleculeStatus::Completed, MoleculeStatus::Collapsed] {
            let tmp = tempfile::tempdir().unwrap();
            let state_dir = tmp.path().join("state");
            let store = FileStore::new(&state_dir);
            let mut m = mol("task-20261017-stuck");
            m.status = status;
            m.energy_budget = Some(cosmon_core::energy::StepBudget {
                cap: 1,
                remaining: 0,
            });
            store.save_molecule(&m.id, &m).unwrap();
            let path = store.molecule_dir(&m.id).join("state.json");
            let before = std::fs::read(&path).unwrap();
            let ctx = Context {
                verbose: false,
                json: true,
                config: Some(state_dir.clone()),
            };
            assert!(run(
                &ctx,
                &Args {
                    molecule: m.id.to_string(),
                    reason: "blocked".into()
                }
            )
            .is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
            assert!(!state_dir.join("events.jsonl").exists());
        }
    }
}
