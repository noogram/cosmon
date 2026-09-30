// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end CLI coverage for the `cs done` **no_branch archival** fix
//! (task-20260626-eb65).
//!
//! THE BUG: a molecule that never produced a mergeable feat branch (a delib, a
//! drainage worker, an empty-branch task) reaches `cs done` `Completed`-but-
//! `archived == false`. The `MergeLoopOutcome::NoBranch` arm pushed only a
//! `no_branch` action and skipped the archive write (it was gated on
//! `merge_succeeded`). `cs done` printed `✅ done`, yet left
//! `{status: Completed, archived: false}` on disk — so the molecule-health A8
//! (`CompletedUnharvested`, ADR-137 §3/§4) re-detected it on *every* sweep as a
//! permanent phantom anomaly. The harvest ran but never *cleared*.
//!
//! THE CONTRACT exercised here, against the **real `cs` binary**:
//!
//!   * a no_branch molecule with `[archive] enabled` ⇒ `cs done` exits zero,
//!     reports both the `no_branch` and `archived` actions, and writes the
//!     archive entry to disk;
//!   * `archived = true` is what makes A8 clearable — proved by the
//!     classifier-side unit test `patrol::tests::test_a8_cleared_once_archived`;
//!   * a second `cs done` is idempotent: it re-reports `already_archived` and
//!     does not rewrite the archive.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

/// `cs` invocation pinned to an isolated state dir and run from inside the
/// project repo (so `find_repo_root()` resolves to the temp git repo).
fn cs_isolated(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let config_path = repo.join(".cosmon/config.toml");
    let mut cmd = cs();
    cmd.env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", &config_path)
        .current_dir(&state_dir);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Init a git repo with a `.cosmon` project whose state is gitignored and
/// whose archive subsystem is **enabled** — the gate the fix flows through.
fn setup_repo(tmp: &Path) {
    git_ok(tmp, &["init", "-q", "-b", "main"]);
    git_ok(tmp, &["config", "user.email", "test@example.com"]);
    git_ok(tmp, &["config", "user.name", "Test"]);
    git_ok(tmp, &["config", "commit.gpgsign", "false"]);

    let cosmon = tmp.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"test-no-branch-archive\"\n\n[archive]\nenabled = true\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();

    fs::write(tmp.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    // One base commit so `main` exists and git topology probes resolve.
    git_ok(tmp, &["add", ".gitignore"]);
    git_ok(tmp, &["commit", "-q", "-m", "base"]);
}

/// Nucleate a `task-work` molecule without creating a feat branch.
fn nucleate_no_branch(repo: &Path) -> String {
    let nuc = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=no_branch archive integration test",
        ])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();

    mol_id
}

/// Drive a molecule to `Completed` without creating a feat branch. `cs
/// complete` does not archive it; that belongs to `cs done`.
fn nucleate_completed_no_branch(repo: &Path) -> String {
    let mol_id = nucleate_no_branch(repo);

    // `--ignore-mindguard`: this temp repo has no surface-verify gate machinery;
    // the hidden test escape hatch keeps the transition hermetic.
    let comp = cs_isolated(repo)
        .args([
            "--json",
            "complete",
            &mol_id,
            "--reason",
            "no_branch test",
            "--ignore-mindguard",
        ])
        .output()
        .expect("cs complete");
    assert!(
        comp.status.success(),
        "complete failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );
    mol_id
}

/// Walk `archive/YYYY/MM/` and return whether an entry for `mol_id` exists.
fn archive_entry_exists(state_dir: &Path, mol_id: &str) -> bool {
    let archive_root = state_dir.join("archive");
    if !archive_root.is_dir() {
        return false;
    }
    for year in fs::read_dir(&archive_root).into_iter().flatten().flatten() {
        if !year.path().is_dir() || year.file_name() == "events" {
            continue;
        }
        for month in fs::read_dir(year.path()).into_iter().flatten().flatten() {
            for entry in fs::read_dir(month.path()).into_iter().flatten().flatten() {
                if entry.file_name() == mol_id {
                    return entry.path().join("molecule.json").is_file();
                }
            }
        }
    }
    false
}

#[test]
fn cs_done_archives_no_branch_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");

    let mol_id = nucleate_completed_no_branch(repo);
    let branch = format!("feat/{mol_id}");

    // Sanity: no feat branch exists — this is genuinely the no_branch path.
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "precondition: no feat branch should exist for a no_branch molecule"
    );

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");

    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);

    // 1. Zero exit — a no_branch teardown is a success, not a failure.
    assert!(
        done.status.success(),
        "cs done must succeed on a no_branch molecule.\nstdout={stdout}\nstderr={stderr}"
    );

    let v: serde_json::Value = serde_json::from_str(stdout.trim().lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("done stdout not JSON: {e}\n{stdout}"));
    let actions: Vec<String> = v["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .map(|a| a.as_str().unwrap_or("").to_owned())
        .collect();

    // 2. Both the no_branch outcome AND the archival are reported.
    assert!(
        actions.iter().any(|a| a == "no_branch"),
        "expected a `no_branch` action, got {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "archived"),
        "expected an `archived` action — the fix: no_branch teardown must archive. got {actions:?}"
    );

    // 3. The archive entry actually landed on disk.
    assert!(
        archive_entry_exists(&state_dir, &mol_id),
        "archive entry for the no_branch molecule must exist on disk"
    );

    // 4. Idempotence: a second `cs done` re-reports already_archived and does
    //    NOT write a second archive action.
    let done2 = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done (replay)");
    assert!(
        done2.status.success(),
        "second cs done must succeed: {}",
        String::from_utf8_lossy(&done2.stderr)
    );
    let stdout2 = String::from_utf8_lossy(&done2.stdout);
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim().lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("replay done stdout not JSON: {e}\n{stdout2}"));
    let actions2: Vec<String> = v2["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .map(|a| a.as_str().unwrap_or("").to_owned())
        .collect();
    assert!(
        actions2.iter().any(|a| a == "already_archived"),
        "second cs done must be a no-op on the archive (already_archived), got {actions2:?}"
    );
}

/// A controller can complete a worker from the state directory. Its session
/// log still belongs to the worker's recorded worktree, and an immediate
/// harvest must archive the resulting observation without a timing delay.
#[test]
fn immediate_done_archives_the_workers_realized_model() {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
    use cosmon_core::id::{MoleculeId, WorkerId};
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{event_log, StateStore, WorkerData};

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let state_dir = repo.join(".cosmon/state");
    let mol_id = nucleate_no_branch(repo);
    let mol = MoleculeId::new(&mol_id).unwrap();
    let worker = WorkerId::new(&mol_id).unwrap();
    let worktree = repo.join(".worktrees").join(&mol_id);
    fs::create_dir_all(&worktree).unwrap();

    let store = FileStore::new(&state_dir);
    let mut data = store.load_molecule(&mol).unwrap();
    data.status = cosmon_core::molecule::MoleculeStatus::Running;
    data.assigned_worker = Some(worker.clone());
    store.save_molecule(&mol, &data).unwrap();
    let mut fleet = cosmon_state::Fleet::default();
    let mut member = WorkerData::new(
        worker.clone(),
        cosmon_core::id::AgentId::new("worker").unwrap(),
        AgentRole::Implementation,
        Clearance::Write,
        WorkerStatus::Active,
    )
    .with_repo(format!(".worktrees/{mol_id}"));
    member.desired = DesiredState::Running;
    fleet.workers.insert(worker.clone(), member);
    store.save_fleet(&fleet).unwrap();

    let log = state_dir.join("events.jsonl");
    event_log::emit_one(
        &log,
        EventV2::AdapterSelected {
            mol_id: mol.clone(),
            adapter_name: "claude".to_owned(),
            selected_at: chrono::Utc::now(),
            selection_source: AdapterSelectionSource::Cli {
                flag: "claude".to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )
    .unwrap();
    event_log::emit_one(
        &log,
        EventV2::WorkerSpawned {
            worker_id: worker,
            molecule: Some(mol),
            session_name: mol_id.clone(),
            role: "worker".to_owned(),
            adapter_name: "claude".to_owned(),
            loop_ownership: Default::default(),
        },
        None,
    )
    .unwrap();

    let config = repo.join("session-config");
    let encoded = cosmon_core::session_thread::sanitise_agent_path(&worktree.to_string_lossy());
    let session_dir = config.join("projects").join(encoded);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("session.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"model\":\"observed-model\"}}\n",
    )
    .unwrap();

    let complete = cs_isolated(repo)
        .env("CLAUDE_CONFIG_DIR", &config)
        .args(["complete", &mol_id, "--ignore-mindguard"])
        .output()
        .unwrap();
    assert!(
        complete.status.success(),
        "complete: {}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let done = cs_isolated(repo)
        .env("CLAUDE_CONFIG_DIR", &config)
        .args(["done", &mol_id, "--no-auto-propel", "--no-worktree-remove"])
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "done: {}",
        String::from_utf8_lossy(&done.stderr)
    );

    let archive_root = state_dir.join("archive");
    let journal = fs::read_dir(&archive_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .flat_map(|year| fs::read_dir(year.path()).unwrap().filter_map(Result::ok))
        .map(|month| month.path().join(&mol_id).join("journal.jsonl"))
        .find(|path| path.is_file())
        .expect("archived journal");
    let rows = fs::read_to_string(journal).unwrap();
    assert!(
        rows.lines().any(|line| {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            row["type"] == "model_observed" && row["model"] == "observed-model"
        }),
        "archive must retain the model observation: {rows}"
    );
}
