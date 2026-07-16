// SPDX-License-Identifier: AGPL-3.0-only

//! Shared fixture for the integration tests — seeds a temp `.cosmon/`
//! project with molecules via a direct [`FileStore`], the same way the
//! crate's internal `test_fixture` module does. Kept under `tests/fixture/`
//! (a subdirectory) so cargo does not compile it as its own test target.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId, StepId, WorkerId};
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};

/// Write the `.cosmon/config.toml` project marker (ADR-069) so the tools'
/// walk-up `state_dir` resolution treats `root` as a cosmon project.
pub fn seed_project(root: &Path) {
    let cosmon = root.join(".cosmon");
    std::fs::create_dir_all(&cosmon).unwrap();
    std::fs::write(
        cosmon.join("config.toml"),
        "# cosmon-ops-tools itest fixture\n",
    )
    .unwrap();
}

/// Seed one untagged molecule with the given lifecycle status.
pub fn seed_molecule(root: &Path, id: &str, status: &str) {
    seed_project(root);
    let store = FileStore::new(root.join(".cosmon").join("state"));
    let data = make_molecule(id, status, &[]);
    store.save_molecule(&data.id.clone(), &data).unwrap();
}

fn make_molecule(id: &str, status: &str, tags: &[&str]) -> MoleculeData {
    let now = Utc::now();
    let tag_set: BTreeSet<Tag> = tags.iter().map(|t| Tag::new(*t).unwrap()).collect();
    MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: status.parse().unwrap(),
        variables: HashMap::new(),
        assigned_worker: Some(WorkerId::new("ruby").unwrap()),
        created_at: now,
        updated_at: now,
        total_steps: 2,
        current_step: 1,
        completed_steps: vec![StepId::new("implement").unwrap()],
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
        tags: tag_set,
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
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    }
}
