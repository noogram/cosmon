// SPDX-License-Identifier: AGPL-3.0-only

//! `merged_at` is stamped by the resident runtime only when the molecule's
//! branch really landed on its base (ADR-041, ADR-052 §I9; Refs #214).
//!
//! The production [`SubprocessExecutor`] runs `cs done <id>` after a molecule
//! completes. A `cs done` that was refused, rolled back or never merged used
//! to be logged as non-fatal and the runtime stamped `merged_at` anyway, which
//! released dependents that could not see the predecessor's work. These tests
//! drive the real executor against a stand-in `cs` script and a real
//! repository built in a temporary directory.

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, DagPolicy, Executor, Runtime, RuntimeConfig, RuntimeError, SubprocessExecutor,
};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("valid molecule id")
}

/// Seed one molecule with an explicit status / `merged_at` / typed links.
fn seed(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
    merged_at: Option<chrono::DateTime<Utc>>,
    links: Vec<MoleculeLink>,
) {
    let data = MoleculeData {
        harvest_reason: None,
        id: id.clone(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: links,
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        base_branch: None,
        protected_paths: Vec::new(),
        pending_step: None,
        merged_at,
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
    };
    store.save_molecule(id, &data).expect("seed molecule");
}

/// Run git in `dir` with a repository-local identity; panics on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with `main` and a `feat/<id>` branch. When `landed` the
/// branch is merged into `main`; otherwise it carries a commit `main` lacks.
fn repo_with_branch(dir: &Path, id: &MoleculeId, landed: bool) {
    git(dir, &["init", "-q"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", &format!("feat/{id}")]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
    git(dir, &["checkout", "-q", "main"]);
    if landed {
        git(
            dir,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "merge",
                &format!("feat/{id}"),
            ],
        );
    }
}

/// A stand-in `cs` that exits with `code` for every invocation.
fn fake_cs(dir: &Path, code: i32) -> PathBuf {
    let path = dir.join("fake-cs.sh");
    std::fs::write(&path, format!("#!/bin/sh\nexit {code}\n")).expect("write fake cs");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// Wraps the real [`SubprocessExecutor`]; its `dispatch` completes the
/// molecule in the store so the loop reaches the completion pass.
struct CompletingSubprocess {
    inner: SubprocessExecutor,
    store_path: PathBuf,
    dispatched: Arc<Mutex<Vec<MoleculeId>>>,
}

impl Executor for CompletingSubprocess {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.dispatched.lock().unwrap().push(id.clone());
        let store = FileStore::new(&self.store_path);
        let mut mol = store.load_molecule(id).map_err(RuntimeError::State)?;
        mol.status = MoleculeStatus::Completed;
        mol.current_step = mol.total_steps;
        mol.updated_at = Utc::now();
        store
            .save_molecule(&mol.id.clone(), &mol)
            .map_err(RuntimeError::State)?;
        Ok(())
    }

    fn on_complete(&self, id: &MoleculeId) -> Result<(), RuntimeError> {
        self.inner.on_complete(id)
    }

    fn branch_landed(&self, mol: &MoleculeData) -> Result<bool, RuntimeError> {
        self.inner.branch_landed(mol)
    }
}

struct Outcome {
    pred: MoleculeData,
    dispatched: Vec<MoleculeId>,
}

/// Drive `pred -> dependent` where `pred` is already `Completed` and unmerged.
/// `cs_exit` is the exit code of the stand-in `cs`; `landed` says whether the
/// branch is merged in the repository; `preset_merged_at` models a `cs done`
/// that recorded `merged_at` itself.
fn run_pred_then_dependent(
    cs_exit: i32,
    landed: bool,
    preset_merged_at: Option<chrono::DateTime<Utc>>,
) -> Outcome {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).expect("repo dir");
    let pred = mol_id("task-20261117-aaaa");
    let dependent = mol_id("task-20261117-bbbb");
    repo_with_branch(&repo, &pred, landed);

    let state = tmp.path().join("state");
    let store = FileStore::new(&state);
    seed(
        &store,
        &pred,
        MoleculeStatus::Completed,
        preset_merged_at,
        vec![MoleculeLink::Blocks {
            target: dependent.clone(),
        }],
    );
    seed(
        &store,
        &dependent,
        MoleculeStatus::Pending,
        None,
        vec![MoleculeLink::BlockedBy {
            source: pred.clone(),
        }],
    );

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&dependent)).expect("compile");
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let executor = CompletingSubprocess {
        inner: SubprocessExecutor::new(&repo)
            .quiet(true)
            .with_cs_program(fake_cs(tmp.path(), cs_exit)),
        store_path: state.clone(),
        dispatched: Arc::clone(&dispatched),
    };
    let config = RuntimeConfig {
        poll_interval: Duration::from_millis(5),
        // The blocked cases never drain; a short bound ends them.
        max_runtime: Some(Duration::from_millis(1500)),
        sweep_orphan_descendants_every: None,
        liveness_recheck_every: None,
    };
    let mut runtime = Runtime::new(
        Box::new(FileStore::new(&state)),
        Box::new(DagPolicy::new(plan, edges)),
        Box::new(executor),
        config,
    );
    runtime.run().expect("runtime run");

    let pred_after = store.load_molecule(&pred).expect("load pred");
    let dispatched = dispatched.lock().unwrap().clone();
    Outcome {
        pred: pred_after,
        dispatched,
    }
}

/// A `cs done` that exits non-zero leaves the molecule un-merged and its
/// dependent is not released by the frontier.
#[test]
fn failed_cs_done_leaves_merged_at_unset_and_dependent_blocked() {
    let out = run_pred_then_dependent(3, true, None);
    assert!(
        out.pred.merged_at.is_none(),
        "merged_at must stay unset when cs done exits non-zero"
    );
    assert!(
        out.dispatched.is_empty(),
        "dependent must not be released, got {:?}",
        out.dispatched
    );
}

/// `cs done` succeeded but git does not show the branch on the base.
#[test]
fn successful_cs_done_with_unreachable_branch_leaves_merged_at_unset() {
    let out = run_pred_then_dependent(0, false, None);
    assert!(
        out.pred.merged_at.is_none(),
        "merged_at must stay unset when the branch tip is not reachable from the base"
    );
    assert!(
        out.dispatched.is_empty(),
        "dependent released: {:?}",
        out.dispatched
    );
}

/// Success path: `cs done` succeeded and the branch is reachable.
#[test]
fn landed_branch_stamps_merged_at_and_releases_dependent() {
    let out = run_pred_then_dependent(0, true, None);
    assert!(
        out.pred.merged_at.is_some(),
        "merged_at must be stamped when the branch landed"
    );
    assert_eq!(out.dispatched.len(), 1, "dependent must be released once");
}

/// A `merged_at` that `cs done` already recorded is not overwritten, even
/// when the branch is not reachable.
#[test]
fn existing_merged_at_is_not_overwritten() {
    let earlier = Utc::now() - chrono::Duration::hours(3);
    let out = run_pred_then_dependent(0, false, Some(earlier));
    assert_eq!(out.pred.merged_at, Some(earlier));
}

/// The executor reports a non-zero `cs done` as an error naming the
/// molecule and the exit code.
#[test]
fn executor_reports_cs_done_exit_code() {
    let tmp = TempDir::new().expect("tempdir");
    let exec = SubprocessExecutor::new(tmp.path())
        .quiet(true)
        .with_cs_program(fake_cs(tmp.path(), 7));
    let id = mol_id("task-20261117-cccc");
    let err = exec
        .on_complete(&id)
        .expect_err("non-zero cs done is an error");
    let text = err.to_string();
    assert!(
        text.contains("task-20261117-cccc") && text.contains('7'),
        "got: {text}"
    );
}
