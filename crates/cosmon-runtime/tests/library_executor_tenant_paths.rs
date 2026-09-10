// SPDX-License-Identifier: AGPL-3.0-only

//! Falsifier for the PR #57 finding: the library executor must read the
//! **tenant's** store, formulas and config — not the ones an inherited
//! environment names.
//!
//! The RPP tackle route authorises and observes a molecule in the admitted
//! tenant's deterministic store (`<tenant_root>/.cosmon/state`) and then
//! hands the tenant root to [`cosmon_runtime::LibraryExecutor`]. While the
//! executor resolved state / formulas / config through the ambient
//! `cosmon_filestore` helpers, `COSMON_STATE_DIR` & co. outranked that
//! root: with a same-named molecule in a second galaxy, the executor loaded
//! and mutated *that* record while dispatching a worker into the tenant's
//! worktree. The worker envelope pins `COSMON_STATE_DIR` for the child and
//! cannot repair a read the parent already made.
//!
//! This file lives in its own test binary **on purpose**: it must run with
//! the cosmon env tiers set, which the sibling `library_executor.rs`
//! fixture deliberately strips process-wide. A stripped fixture cannot see
//! this property at all — it can only fail to notice its absence.
//!
//! All three halves live in ONE `#[test]`: the environment is
//! process-global, so two tests poisoning it concurrently would read each
//! other's values.
//!
//! Both sides of the contract are asserted, because the fix is a *choice
//! of resolution*, not a removal of one:
//!
//! * `LibraryExecutor::new` alone keeps ambient precedence — the CLI-shaped
//!   caller whose operator exported an override is asking for it. Under the
//!   poisoned environment it dispatches the decoy record. This is the
//!   historical behaviour, and it is what made the finding reproducible.
//! * `.with_paths(TenantPaths::rooted_at(root))` pins the tenant. Under the
//!   same poisoned environment the tenant's own record is the one that
//!   moves, and the decoy is untouched.

use std::path::{Path, PathBuf};

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{DispatchPin, Executor, LibraryExecutor, TenantPaths};
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::mock::MockBackend;

/// A pending molecule in the canonical fleet shape.
fn pending_molecule(id: &str) -> MoleculeData {
    let now = chrono::Utc::now();
    MoleculeData {
        id: MoleculeId::new(id).expect("id"),
        fleet_id: cosmon_core::id::FleetId::new("default").expect("fleet"),
        formula_id: cosmon_core::id::FormulaId::new("task-work").expect("formula"),
        status: MoleculeStatus::Pending,
        variables: std::collections::HashMap::new(),
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
        tags: std::collections::BTreeSet::new(),
        escalations: vec![],
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        pending_step: None,
        merged_at: None,
        non_integration: None,
        // Nothing has closed this fixture — it is `Pending`, and the door
        // that records a harvest reason has not run. An honest absence.
        harvest_reason: None,
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

/// A throwaway galaxy: a git repository whose root carries `.cosmon/` with
/// a config marker (the walk-up anchor) and a state directory.
fn galaxy(root: &Path, name: &str) -> PathBuf {
    let project = root.join(name);
    std::fs::create_dir_all(&project).expect("project dir");
    let init = std::process::Command::new("git")
        .args(["-C", &project.to_string_lossy(), "init", "-q"])
        .status()
        .expect("git init");
    assert!(init.success(), "git init must succeed");
    std::fs::create_dir_all(project.join(".cosmon").join("state")).expect("state dir");
    std::fs::write(project.join(".cosmon").join("config.toml"), "").expect("config marker");
    project
}

/// The state store of a galaxy built by [`galaxy`].
fn store_of(project: &Path) -> FileStore {
    FileStore::new(project.join(".cosmon").join("state"))
}

/// Seed the same molecule id in both galaxies, so the only thing that can
/// tell the two records apart is *which store the dispatch touched*.
fn seed_both(tenant: &Path, decoy: &Path, id: &str) -> MoleculeId {
    let mol = pending_molecule(id);
    store_of(tenant)
        .save_molecule(&mol.id, &mol)
        .expect("seed tenant molecule");
    store_of(decoy)
        .save_molecule(&mol.id, &mol)
        .expect("seed decoy molecule");
    mol.id
}

/// Point every cosmon env tier at the decoy galaxy — the poisoned parent
/// environment an RPP adapter can inherit from whatever started it.
fn poison_env_with(decoy: &Path) {
    std::env::set_var("PATH", "/usr/bin:/bin");
    std::env::set_var("COSMON_STATE_DIR", decoy.join(".cosmon").join("state"));
    std::env::set_var(
        "COSMON_FORMULAS_DIR",
        decoy.join(".cosmon").join("formulas"),
    );
    std::env::set_var("COSMON_CONFIG", decoy.join(".cosmon").join("config.toml"));
}

/// Whether a dispatch landed on this store: `Running` with a bound process
/// record is the ledger's own witness that the worker was recorded here.
fn was_dispatched(project: &Path, id: &MoleculeId) -> bool {
    let mol = store_of(project).load_molecule(id).expect("re-read");
    mol.status == MoleculeStatus::Running && mol.process.is_some()
}

#[test]
fn explicit_tenant_paths_beat_a_poisoned_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tenant = galaxy(dir.path(), "tenant");
    let decoy = galaxy(dir.path(), "decoy");
    poison_env_with(&decoy);

    // --- Half 1: ambient resolution still obeys the environment. This is
    //     the reproduction of the finding, and the behaviour the CLI wants.
    let ambient_id = seed_both(&tenant, &decoy, "task-20260909-a1a1");
    LibraryExecutor::new(&tenant, MockBackend::new())
        .dispatch_with_pin(&ambient_id, &DispatchPin::default())
        .expect("the ambient dispatch must run");
    assert!(
        was_dispatched(&decoy, &ambient_id),
        "ambient resolution must follow COSMON_STATE_DIR — otherwise this \
         test no longer reproduces the confusion it guards against"
    );
    assert!(
        !was_dispatched(&tenant, &ambient_id),
        "the ambient dispatch read the decoy store, so the tenant's own \
         record cannot have moved"
    );
    // …while the worker it dispatched went into the TENANT's worktree: the
    // record and the worktree come from two different galaxies, which is
    // exactly the shape the finding names.
    assert!(
        tenant.join(".worktrees").join(ambient_id.as_str()).exists(),
        "the ambient dispatch put its worktree under the cwd galaxy"
    );

    // --- Half 2: the pinned executor ignores the same poisoned env.
    let pinned_id = seed_both(&tenant, &decoy, "task-20260909-b2b2");
    LibraryExecutor::new(&tenant, MockBackend::new())
        .with_paths(TenantPaths::rooted_at(&tenant))
        .dispatch_with_pin(&pinned_id, &DispatchPin::default())
        .expect("the pinned dispatch must run against the tenant store");
    assert!(
        was_dispatched(&tenant, &pinned_id),
        "with TenantPaths::rooted_at, the tenant's own record is the one \
         that is loaded and mutated"
    );
    assert!(
        !was_dispatched(&decoy, &pinned_id),
        "the store named by COSMON_STATE_DIR must be untouched by a pinned \
         dispatch — a mutation there is the finding, unfixed"
    );

    // --- Half 3: the other failure mode of the same confusion. With no
    //     same-named record in the poisoned store, the ambient path cannot
    //     find an authorised tenant molecule at all; the pinned path can.
    let mol = pending_molecule("task-20260909-c3c3");
    store_of(&tenant)
        .save_molecule(&mol.id, &mol)
        .expect("seed tenant only");

    let ambient = LibraryExecutor::new(&tenant, MockBackend::new())
        .dispatch_with_pin(&mol.id, &DispatchPin::default());
    assert!(
        ambient.is_err(),
        "ambient resolution reads the decoy store, where this molecule does \
         not exist — the authorised molecule fails unexpectedly"
    );

    LibraryExecutor::new(&tenant, MockBackend::new())
        .with_paths(TenantPaths::rooted_at(&tenant))
        .dispatch_with_pin(&mol.id, &DispatchPin::default())
        .expect("the pinned dispatch resolves the tenant's own molecule");
    assert!(was_dispatched(&tenant, &mol.id));
}
