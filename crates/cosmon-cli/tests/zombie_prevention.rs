// SPDX-License-Identifier: AGPL-3.0-only

//! Zombie-prevention integration tests for `cs tackle`.
//!
//! These guard the class of "surface-lie" bugs where `cs tackle` wrote
//! `molecule.status = Running` + a fleet `WorkerData` entry even though
//! the underlying `claude` process never came alive.
//!
//! The test substitutes a `sh -c 'exit 1'` script for `claude` on `PATH`,
//! runs `cs tackle <mol-id>`, and asserts that the surface stays truthful:
//!   - `cs tackle` exits non-zero;
//!   - the molecule's `state.json` is NOT `running`;
//!   - no `WorkerData` row points at the molecule;
//!   - the tmux session is gone (no alive pane, no `[exited]` carcass);
//!   - the `feat/<mol-id>` branch is NOT left behind.

#![cfg(unix)]

use std::fs;
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn git(project: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

/// Install a `claude` binary on the test's PATH that simulates the
/// failure mode under investigation (silent exec failure, non-zero
/// exit). Returns the bin dir so the caller can prepend it to PATH.
fn install_fake_claude_exit1(tmp: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tmp.join("fakebin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake = bin_dir.join("claude");
    fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
    let mut perm = fs::metadata(&fake).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&fake, perm).unwrap();
    bin_dir
}

#[test]
#[allow(clippy::too_many_lines)]
fn cs_tackle_refuses_to_lie_when_claude_exits_nonzero() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("fixture");
    fs::create_dir_all(&project).unwrap();

    // 1. `cs init --yes` creates .cosmon/ + project_id + builtin formulas.
    //    It does NOT run `git init` — that is the user's job.
    let out = cosmon_bin()
        .args(["init", "--yes"])
        .current_dir(&project)
        .output()
        .expect("cs init");
    assert!(
        out.status.success(),
        "cs init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Bootstrap git ourselves, configure identity, and stamp an initial
    //    commit so `git branch` + `git worktree add` have a HEAD to work
    //    from.
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "test@test.local"]);
    git(&project, &["config", "user.name", "cosmon-test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init"]);

    // 3. Nucleate a task-work molecule we'll try to tackle.
    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=zombie-test",
        ])
        .current_dir(&project)
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = parsed["id"]
        .as_str()
        .expect("nucleate should return id")
        .to_owned();

    // 4. Put a deliberately broken claude on PATH.
    let bin_dir = install_fake_claude_exit1(tmp.path());
    let current_path = std::env::var("PATH").unwrap_or_default();
    let injected_path = format!("{}:{current_path}", bin_dir.display());

    // 5. `cs tackle` must fail — the evidence-based readiness check
    //    should observe no live claude and refuse to write Running.
    //    `--adapter claude` is explicit since task-20260530-821f flipped
    //    the built-in default to `local`; this test guards the claude
    //    tmux-readiness path specifically.
    let tackle_out = cosmon_bin()
        .args(["tackle", &mol_id, "--adapter", "claude"])
        .current_dir(&project)
        .env("PATH", &injected_path)
        .output()
        .expect("cs tackle");
    let tackle_stdout = String::from_utf8_lossy(&tackle_out.stdout).into_owned();
    let tackle_stderr = String::from_utf8_lossy(&tackle_out.stderr).into_owned();

    // 5a. Non-zero exit — the critical regression assertion.
    assert!(
        !tackle_out.status.success(),
        "cs tackle MUST fail when claude exits 1.\nstdout: {tackle_stdout}\nstderr: {tackle_stderr}"
    );

    // 5b. Molecule must NOT be `running`.
    let state_path = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(&mol_id)
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).expect("read state.json")).unwrap();
    let status = state["status"].as_str().unwrap_or("");
    assert_ne!(
        status, "running",
        "molecule {mol_id} is {status} after failed tackle — surface lie"
    );

    // 5c. Fleet must not contain a worker bound to this molecule.
    let fleet_path = project.join(".cosmon/state/fleet.json");
    if fleet_path.exists() {
        let fleet: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fleet_path).unwrap()).unwrap();
        if let Some(workers) = fleet.get("workers").and_then(|v| v.as_object()) {
            for (wid, w) in workers {
                let cur = w.get("current_molecule").and_then(|v| v.as_str());
                assert_ne!(
                    cur,
                    Some(mol_id.as_str()),
                    "fleet worker {wid} is bound to molecule {mol_id} after failed tackle"
                );
            }
        }
    }

    // 5d. No leftover tmux session (alive OR dead-carcass) on the
    //     project's dedicated socket.
    let config_path = project.join(".cosmon/config.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let project_id = config["project"]["project_id"]
        .as_str()
        .expect("project_id")
        .to_owned();
    let tmux_out = Command::new("tmux")
        .args([
            "-L",
            &project_id,
            "list-panes",
            "-a",
            "-F",
            "#{session_name}|#{pane_dead}",
        ])
        .output()
        .expect("tmux list-panes");
    let panes_stdout = String::from_utf8_lossy(&tmux_out.stdout);
    for line in panes_stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        assert!(
            !line.contains("zombie-test"),
            "leftover tmux pane for the molecule: {line}"
        );
    }
    let _ = Command::new("tmux")
        .args(["-L", &project_id, "kill-server"])
        .output();

    // 5e. No orphan `feat/<mol-id>` branch. `cs tackle` must either
    //     succeed end-to-end (branch kept, worker registered) or leave
    //     the tree in its pre-invocation state.
    let branch_out = Command::new("git")
        .args(["branch", "--list", &format!("feat/{mol_id}")])
        .current_dir(&project)
        .output()
        .expect("git branch --list");
    let branches = String::from_utf8_lossy(&branch_out.stdout);
    assert!(
        branches.trim().is_empty(),
        "orphan branch feat/{mol_id} left behind after failed tackle: {branches:?}"
    );
}
