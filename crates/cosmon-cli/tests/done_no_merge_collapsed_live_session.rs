// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #155, item 3: `cs done --no-merge` on a **collapsed** molecule must
//! kill the live recorded tmux session and purge its worker, exactly as the
//! `--dry-run` plan announces.
//!
//! `cs collapse` releases the molecule's live-process record, which also
//! nulls `session_name`. Teardown used to fall back to the molecule id, so a
//! worker whose session carries a functional name (`<slug>-<short id>`, the
//! normal case) was neither killed nor purged: the worktree vanished under a
//! running worker.
//!
//! The rig owns a tmux server on a private socket (`-L <unique>`), torn down
//! by [`rig_guard::TmuxServer`] on every exit path.

#[allow(dead_code)]
mod rig_guard;

use std::fs;
use std::path::Path;
use std::process::Command;

use rig_guard::TmuxServer;

fn git_ok(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn failed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cs(repo: &Path) -> Command {
    let state_dir = repo.join(".cosmon/state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_STATE_DIR", &state_dir)
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(&state_dir);
    cmd
}

/// A project whose tmux socket name (its `project_id`) is unique to this run.
fn setup_repo(repo: &Path, socket: &str) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        format!("[project]\nproject_id = \"{socket}\"\n\n[archive]\nenabled = true\n"),
    )
    .unwrap();
    fs::write(
        cosmon.join("state/fleet.json"),
        "{\"repos\":{},\"workers\":{}}",
    )
    .unwrap();
    let formula =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(formula, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    git_ok(repo, &["add", ".gitignore"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
}

fn molecule_state_path(repo: &Path, mol_id: &str) -> std::path::PathBuf {
    repo.join(".cosmon/state/fleets/default/molecules")
        .join(mol_id)
        .join("state.json")
}

#[test]
fn done_no_merge_on_a_collapsed_molecule_kills_the_recorded_session_and_purges_the_worker() {
    let socket = format!(
        "nm{}-{:04x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() & 0xffff)
    );
    let tmux = TmuxServer::new(&socket);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo, &socket);

    let nuc = cs(repo)
        .args(["--json", "nucleate", "task-work", "--var", "topic=live"])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "{}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().unwrap().to_owned();

    // What `cs tackle` records: a functional session name, bound as the
    // molecule's live process and as its fleet worker.
    let session = format!("live-worker-{}", mol_id.rsplit('-').next().unwrap());
    let state_path = molecule_state_path(repo, &mol_id);
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["session_name"] = session.clone().into();
    state["assigned_worker"] = session.clone().into();
    state["process"] = serde_json::json!({
        "worker_id": session,
        "tmux_session": session,
        "started_at": "2026-10-02T00:00:00Z",
        "status": "active",
    });
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    fs::write(
        repo.join(".cosmon/state/fleet.json"),
        serde_json::json!({
            "repos": {},
            "workers": { session.clone(): {
                "agent_id": "tackle", "clearance": "write", "current_molecule": mol_id,
                "desired": "running", "id": session, "role": "implementation",
                "status": "active", "updated_at": "2026-10-02T00:00:00Z",
                "worker_role": "cognition",
            }},
        })
        .to_string(),
    )
    .unwrap();

    let started = tmux.run(&["new-session", "-d", "-s", &session, "sleep 300"]);
    assert!(started.status.success(), "tmux new-session failed");
    assert!(tmux.has_session(&session), "precondition: session is live");

    let collapse = cs(repo)
        .args([
            "collapse",
            &mol_id,
            "--reason",
            "abandoned",
            "--reason-kind",
            "blocker_stuck",
        ])
        .output()
        .expect("cs collapse");
    assert!(
        collapse.status.success(),
        "{}",
        String::from_utf8_lossy(&collapse.stderr)
    );

    // The plan must announce what the run then does.
    let plan = cs(repo)
        .args(["--json", "done", &mol_id, "--no-merge", "--dry-run"])
        .output()
        .expect("cs done --dry-run");
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(
        plan["session_alive"], true,
        "dry-run must see the live session: {plan:#}"
    );

    let done = cs(repo)
        .args(["--json", "done", &mol_id, "--no-merge", "--no-auto-propel"])
        .output()
        .expect("cs done --no-merge");
    let out = String::from_utf8_lossy(&done.stdout).into_owned();

    assert!(
        !tmux.has_session(&session),
        "the live recorded session must be killed by `cs done --no-merge`; output: {out}"
    );
    let fleet: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join(".cosmon/state/fleet.json")).unwrap()).unwrap();
    assert!(
        fleet["workers"].get(&session).is_none(),
        "the worker must be purged from the fleet; fleet: {fleet:#}"
    );
}
