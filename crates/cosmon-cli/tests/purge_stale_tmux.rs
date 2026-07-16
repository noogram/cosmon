// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — `cs purge` must probe
//! tmux liveness before declaring "nothing to purge".
//!
//! Scenario: a worker is tackled (fleet says `desired=Running`,
//! tmux session alive). We externally kill its tmux session, leaving
//! fleet state and reality out of sync — exactly the surface-lie bug
//! the task addresses. Without the liveness probe, `cs purge` would
//! read fleet.json, find no `desired=Stopped` workers, and report
//! "nothing to purge" while the ghost entry lingered.
//!
//! `DoD` assertions:
//!
//! 1. `cs purge` reclaims the ghost entry from `fleet.json` after
//!    the tmux session has been killed (the central regression).
//! 2. `events.jsonl` carries a `WorkerKilled` record whose `reason`
//!    names the stale-tmux population — tooling downstream (events,
//!    overseer, chronicle sweep) can tell reclamation apart from a
//!    normal `desired=Stopped` purge.
//! 3. A still-live worker on the same socket is NOT reclaimed (the
//!    probe is per-worker, not a blanket wipe).
//!
//! `#[ignore]`'d because it spawns real tmux sessions and writes
//! under a temporary git fixture. Run with:
//!
//! ```bash
//! cargo test -p cosmon-cli --test purge_stale_tmux -- --ignored
//! ```

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

/// Install a fake `claude` that prints `❯` (satisfies tackle's
/// readiness check) and then sleeps so the session stays alive until
/// the test kills it. Same pattern as `pane_died_hook.rs`.
fn install_fake_claude(tmp: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tmp.join("fakebin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake = bin_dir.join("claude");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '\\xe2\\x9d\\xaf\\n'\nexec sleep 600\n",
    )
    .unwrap();
    let mut perm = fs::metadata(&fake).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&fake, perm).unwrap();
    bin_dir
}

/// Initialise a cosmon fixture (init + git commit) and return its path
/// and project id (which doubles as the tmux socket name).
fn init_fixture() -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("fixture");
    fs::create_dir_all(&project).unwrap();

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

    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "test@test.local"]);
    git(&project, &["config", "user.name", "cosmon-test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init"]);

    let config_path = project.join(".cosmon/config.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let project_id = config["project"]["project_id"]
        .as_str()
        .expect("project_id")
        .to_owned();

    (tmp, project, project_id)
}

/// Nucleate + tackle a molecule under the fixture, returning the
/// molecule id and its tmux session name (as tackle stamped it).
fn tackle_molecule(project: &Path, fake_bin_dir: &Path, topic: &str) -> (String, String) {
    let out = cosmon_bin()
        .args(["--json", "nucleate", "task-work", "--var"])
        .arg(format!("topic={topic}"))
        .current_dir(project)
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "cs nucleate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = parsed["id"].as_str().expect("nucleate id").to_owned();

    let current_path = std::env::var("PATH").unwrap_or_default();
    let injected_path = format!("{}:{current_path}", fake_bin_dir.display());

    // `--adapter claude` is now explicit: since task-20260530-821f the
    // built-in default is `local` (in-process Ollama), so a bare
    // `cs tackle` would no longer create the tmux session this
    // stale-purge test depends on.
    let out = cosmon_bin()
        .args(["tackle", &mol_id, "--adapter", "claude"])
        .current_dir(project)
        .env("PATH", &injected_path)
        .output()
        .expect("cs tackle");
    assert!(
        out.status.success(),
        "cs tackle failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let state_path = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(&mol_id)
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let session_name = state["session_name"]
        .as_str()
        .expect("state.session_name set after tackle")
        .to_owned();

    (mol_id, session_name)
}

fn fleet_has_worker(project: &Path, session_name: &str) -> bool {
    let fleet_path = project.join(".cosmon/state/fleet.json");
    let Ok(s) = fs::read_to_string(&fleet_path) else {
        return false;
    };
    let Ok(fleet) = serde_json::from_str::<serde_json::Value>(&s) else {
        return false;
    };
    fleet
        .get("workers")
        .and_then(|v| v.as_object())
        .is_some_and(|map| map.contains_key(session_name))
}

fn events_contain(project: &Path, needle: &str) -> bool {
    let events_path = project.join(".cosmon/state/events.jsonl");
    fs::read_to_string(&events_path)
        .map(|s| s.contains(needle))
        .unwrap_or(false)
}

#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn purge_reclaims_worker_after_tmux_session_killed() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (tmp, project, project_id) = init_fixture();
    let bin_dir = install_fake_claude(tmp.path());

    // Two molecules: ghost (we will kill its tmux) + survivor (stays
    // alive, must NOT be reclaimed by the sweep).
    let (ghost_id, ghost_session) = tackle_molecule(&project, &bin_dir, "ghost");
    let (_survivor_id, survivor_session) = tackle_molecule(&project, &bin_dir, "survivor");

    assert!(
        fleet_has_worker(&project, &ghost_session),
        "ghost worker should be registered after tackle"
    );
    assert!(
        fleet_has_worker(&project, &survivor_session),
        "survivor worker should be registered after tackle"
    );

    // Kill the tmux session out from under the fleet record. This is
    // the operational failure mode task-20260419-5982 exists to
    // address: fleet says `desired=Running`, tmux says the session is
    // gone, and the old `cs purge` said "nothing to purge".
    let kill = Command::new("tmux")
        .args(["-L", &project_id, "kill-session", "-t", &ghost_session])
        .output()
        .expect("tmux kill-session");
    assert!(
        kill.status.success(),
        "kill-session failed: stderr={}",
        String::from_utf8_lossy(&kill.stderr)
    );

    // Run `cs purge` — the liveness probe must reclassify the ghost
    // to Stale and remove it.
    let out = cosmon_bin()
        .args(["--json", "purge"])
        .current_dir(&project)
        .output()
        .expect("cs purge");
    assert!(
        out.status.success(),
        "cs purge failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let stale_list = parsed["stale"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    assert!(
        stale_list.iter().any(|s| s == &ghost_session),
        "cs purge --json output must list the ghost session under \"stale\"; \
         got stale={stale_list:?} full={parsed}"
    );

    // DoD (1): fleet.json no longer carries the ghost entry.
    assert!(
        !fleet_has_worker(&project, &ghost_session),
        "ghost worker must be reclaimed from fleet.json (molecule {ghost_id})"
    );
    // DoD (3): survivor is untouched — the sweep is surgical.
    assert!(
        fleet_has_worker(&project, &survivor_session),
        "survivor worker must NOT be reclaimed — its tmux session is alive"
    );

    // DoD (2): events.jsonl names the stale-tmux population so
    // downstream tooling can tell it apart from routine purges.
    assert!(
        events_contain(&project, "stale tmux"),
        "events.jsonl must record the stale-tmux reason string"
    );
    assert!(
        events_contain(&project, &ghost_session),
        "events.jsonl must mention the reclaimed worker id"
    );

    // Cleanup: kill what remains so the test doesn't leak tmux state.
    let _ = Command::new("tmux")
        .args(["-L", &project_id, "kill-server"])
        .output();
}
