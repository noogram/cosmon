// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for ADR-052 child #4 — the mandatory tmux
//! `pane-died` hook.
//!
//! The test stands up a full cosmon fixture (`cs init` + git + a
//! long-running fake `claude`), runs `cs tackle`, kills the tmux pane,
//! and asserts the three `DoD` properties of child #4:
//!
//! 1. A [`EventV2::WorkerExited`] event lands in `events.jsonl`
//!    within 2 seconds of the pane dying (I4 + I8 — event-driven
//!    liveness, emission before action).
//! 2. The projected [`RunState::witness.process`] transitions to
//!    [`Liveness::Dead`] (I10 — silence-as-signal, the external
//!    witness is what makes the transition observable).
//! 3. When the molecule was already `Completed` at pane-death time,
//!    `fleet.json` purges the entry after the auto-harvest fires the
//!    sibling `cs done` (I5 — `CompletedEventuallyMerges`, the weak-
//!    fairness obligation on Harvest).
//!
//! The test is `#[ignore]`'d because it spawns real tmux sessions and
//! real git worktrees. Run with:
//!
//! ```bash
//! cargo test -p cosmon-cli --test pane_died_hook -- --ignored
//! ```
//!
//! # Why a full `cs init` + `cs tackle`
//!
//! A narrower test could call `TmuxBackend::install_pane_died_hook`
//! directly with a crafted command, but that would not exercise the
//! tackle-time wiring — and tackle is exactly the surface ADR-052 #4
//! promotes from best-effort to mandatory. A regression that silently
//! stops installing the hook (e.g. a refactor that forgets to wire it
//! into a new tackle path) must show up here.
//!
//! # Why a fake claude
//!
//! `cs tackle`'s readiness check (see
//! `cosmon-transport::readiness::classify_output`) waits for a `❯`
//! prompt or "Type your message" on the pane. A trivial `sleep`
//! process would never print either and tackle would fail. The fake
//! emits `❯` once, then `sleep`s — enough to pass readiness and stay
//! alive until we kill the pane.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

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

/// A fake `claude` that prints a ready prompt (`❯`) then sleeps until
/// killed. Satisfies `cs tackle`'s readiness check while staying alive
/// long enough for the test to exercise `pane-died`.
fn install_fake_claude_alive(tmp: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tmp.join("fakebin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake = bin_dir.join("claude");
    // `printf` keeps the prompt on the pane. `exec sleep` replaces the
    // shell so `pane-died` fires when the test kills the process, not
    // when the shell finishes spawning.
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

/// Look up the OS pid of the (single) pane in `session_name`.
fn pane_pid(socket: &str, session_name: &str) -> Option<i32> {
    let out = Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_pid}",
        ])
        .output()
        .ok()?;
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse::<i32>()
        .ok()
}

/// SIGKILL the worker grand-child by killing its pane process — the
/// faithful "kill -9 the detached worker" path.
///
/// Unlike `tmux kill-session` (an administrative teardown that does NOT
/// fire `pane-died`), killing the pane *process* makes tmux observe the
/// death and run the `pane-died` hook. This only works because the
/// session is armed with `remain-on-exit on` at hook-install time — the
/// C2 fix without which the kernel-level witness never fires.
fn kill_pane_process(socket: &str, session_name: &str) -> bool {
    let Some(pid) = pane_pid(socket, session_name) else {
        return false;
    };
    Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

/// True once the session's pane has died (`pane_dead=1`) — the carcass
/// `remain-on-exit on` leaves after the worker process exits. The cosmon
/// liveness check (`parse_pane_listing`) treats such a session as not
/// alive even though `tmux has-session` still succeeds.
fn pane_is_dead(socket: &str, session_name: &str) -> bool {
    Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-panes",
            "-t",
            session_name,
            "-F",
            "#{pane_dead}",
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|s| !s.trim().is_empty() && s.lines().all(|l| l.trim() == "1"))
}

/// Read every JSONL line in `events_path`, returning raw JSON values.
fn read_events(events_path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(events_path)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Poll `events.jsonl` up to `timeout` for a line whose event `type` is
/// `worker_exited` and whose `molecule_id` matches `expected`.
fn wait_for_worker_exited(
    events_path: &Path,
    expected: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let events = read_events(events_path);
        if let Some(ev) = events.into_iter().find(|e| {
            e.get("type").and_then(|v| v.as_str()) == Some("worker_exited")
                && e.get("molecule_id").and_then(|v| v.as_str()) == Some(expected)
        }) {
            return Some(ev);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Setup a cosmon fixture + fake claude + nucleated molecule, then run
/// `cs tackle`. Returns the project dir, molecule id, project id
/// (tmux socket) and session name on success, or panics.
fn tackle_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    String,
    String,
    String,
) {
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

    let out = cosmon_bin()
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=pane-died-test",
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
    let mol_id = parsed["id"].as_str().expect("nucleate id").to_owned();

    let bin_dir = install_fake_claude_alive(tmp.path());
    let current_path = std::env::var("PATH").unwrap_or_default();
    let injected_path = format!("{}:{current_path}", bin_dir.display());

    // `--adapter claude` is now explicit: since task-20260530-821f the
    // built-in default is `local` (in-process Ollama), so a bare
    // `cs tackle` would no longer exercise the claude/tmux pane-death
    // path this test guards.
    let out = cosmon_bin()
        .args(["tackle", &mol_id, "--adapter", "claude"])
        .current_dir(&project)
        .env("PATH", &injected_path)
        .output()
        .expect("cs tackle");
    assert!(
        out.status.success(),
        "cs tackle failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let config_path = project.join(".cosmon/config.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let project_id = config["project"]["project_id"]
        .as_str()
        .expect("project_id")
        .to_owned();

    // Resolve the session name tackle chose. Session name stamping is an
    // implementation detail — read it back from state.json rather than
    // reconstructing it so a refactor doesn't silently break this test.
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

    (tmp, project, mol_id, project_id, session_name)
}

/// `DoD` (a) + (b): pane-died emits `WorkerExited` within 2s and the
/// projected `RunState.witness.process` transitions to `Dead`.
///
/// The molecule is still `Running` at pane-death time, so `cs harvest`
/// is a no-op on the merge side (status != Completed). The assertion
/// is strictly about the event + the projected witness — (c) is the
/// sibling test.
#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn pane_died_emits_worker_exited_and_projects_dead_witness() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (_tmp, project, mol_id, project_id, session_name) = tackle_fixture();

    // Kill the worker process — this is the event ADR-052 #4 promises to
    // catch. We kill the pane *process* (not the session) so `pane-died`
    // actually fires (see `kill_pane_process`).
    assert!(
        kill_pane_process(&project_id, &session_name),
        "kill -9 of the pane process must succeed"
    );

    let events_path = project.join(".cosmon/state/events.jsonl");

    // (a) WorkerExited appears within 2 seconds.
    let ev = wait_for_worker_exited(&events_path, &mol_id, Duration::from_secs(5))
        .expect("WorkerExited event must land in events.jsonl within 5s of pane-died");
    assert_eq!(ev["type"].as_str(), Some("worker_exited"));
    assert_eq!(ev["reason"].as_str(), Some("pane_died"));

    // (b) Project RunState and assert witness.process = Dead.
    //
    // We can't read a persisted RunState — child #1 still runs on a
    // feature flag — but the projection function is what `cs peek`
    // and `cs observe` consume, so testing it is the operator-facing
    // contract. Rebuild it from the same inputs those commands see.
    //
    // `remain-on-exit on` keeps the session as a carcass, so liveness is
    // "the pane is dead", not "the session is gone" — the cosmon
    // `parse_pane_listing` check reads it as not-alive all the same.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !pane_is_dead(&project_id, &session_name) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        pane_is_dead(&project_id, &session_name),
        "pane in {session_name} must be dead after kill -9"
    );

    let transport = cosmon_core::worker::TransportState::Dead;
    let state_path = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(&mol_id)
        .join("state.json");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let status_str = state["status"].as_str().unwrap_or("");
    let mol_status = match status_str {
        "running" => cosmon_core::molecule::MoleculeStatus::Running,
        "completed" => cosmon_core::molecule::MoleculeStatus::Completed,
        "pending" => cosmon_core::molecule::MoleculeStatus::Pending,
        other => panic!("unexpected molecule status {other:?}"),
    };
    let run_state =
        cosmon_core::run_state::project_run_state(mol_status, transport, None, chrono::Utc::now());
    let witness = run_state.witness.as_ref().expect("witness projected");
    assert_eq!(
        witness.process,
        cosmon_core::run_state::Liveness::Dead,
        "RunState.witness.process must be Dead after pane-died: {witness:?}"
    );
    // And the ghost must be DeadPane (the dfd8 shape) since the
    // molecule is still pilot-intent Run.
    if matches!(run_state.intent, cosmon_core::run_state::Intent::Run) {
        assert_eq!(
            run_state.ghost(chrono::Utc::now(), Duration::from_secs(90)),
            Some(cosmon_core::run_state::GhostKind::DeadPane),
            "Run intent + Dead witness must classify as DeadPane"
        );
    }

    // Cleanup.
    let _ = Command::new("tmux")
        .args(["-L", &project_id, "kill-server"])
        .output();
}

/// The kill -9 path. `kill -9` the
/// detached grand-child (the claude pane process), then assert the two
/// observed (not declared) properties the briefing demands:
///
/// 1. the `pane-died` hook persists `<mol_dir>/worker.exit` with a
///    non-zero code (the wait-status of a SIGKILL'd process);
/// 2. `MoleculeProcess.status` in `state.json` transitions to `stale`
///    (Dead witness → two-coup projection → Stale, in one coup).
///
/// This exercises the full chain: tmux pane-death → `cs harvest
/// --from-pane-died` → `record_pane_died` → `write_worker_exit` +
/// `project_dead_onto_process_status`. The unit tests in
/// `cmd/harvest.rs` pin the logic deterministically; this proves the
/// tmux wiring actually fires it.
#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn kill_dash_nine_writes_worker_exit_and_stales_process_status() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (_tmp, project, mol_id, project_id, session_name) = tackle_fixture();

    // SIGKILL the detached grand-child by killing its pane process — the
    // faithful "kill -9 the grand-child" the briefing names (vs killing
    // the whole session, which would not fire `pane-died`).
    assert!(
        kill_pane_process(&project_id, &session_name),
        "kill -9 of the pane process must succeed"
    );

    let mol_dir = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(&mol_id);
    let exit_path = mol_dir.join("worker.exit");
    let state_path = mol_dir.join("state.json");

    // Poll for both effects (the hook is event-driven but async).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exit_body = String::new();
    let mut process_status = String::new();
    while Instant::now() < deadline {
        if let Ok(s) = fs::read_to_string(&exit_path) {
            exit_body = s.trim().to_owned();
        }
        if let Ok(s) = fs::read_to_string(&state_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                process_status = v
                    .get("process")
                    .and_then(|p| p.get("status"))
                    .and_then(|st| st.as_str())
                    .unwrap_or("")
                    .to_owned();
            }
        }
        if !exit_body.is_empty() && process_status == "stale" {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    // (1) worker.exit written, non-zero.
    assert!(
        !exit_body.is_empty(),
        "worker.exit must be written by the pane-died hook"
    );
    assert_ne!(
        exit_body, "0",
        "a SIGKILL'd worker must record a non-zero worker.exit, got {exit_body:?}"
    );

    // (2) process.status transitioned to Stale (observed, not declared).
    assert_eq!(
        process_status, "stale",
        "MoleculeProcess.status must transition to stale after a hard pane-death"
    );

    let _ = Command::new("tmux")
        .args(["-L", &project_id, "kill-server"])
        .output();
}

/// `DoD` (c): when the molecule is Completed at pane-death time, the
/// pane-died hook's `cs harvest` invokes `cs done`, which merges the
/// branch and purges the fleet entry.
#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn pane_died_triggers_harvest_which_purges_fleet_when_completed() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (_tmp, project, mol_id, project_id, session_name) = tackle_fixture();

    // Manually transition the molecule to Completed BEFORE killing the
    // pane, so `cs harvest --if-completed` takes the exec-`cs done`
    // branch. We write state.json directly — it is the source of truth
    // (ADR-052 I1) and harvest reads it straight.
    let state_path = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(&mol_id)
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    state["status"] = serde_json::Value::String("completed".to_owned());
    state["merged_at"] = serde_json::Value::Null;
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Kill the worker process → pane-died hook fires → cs harvest → cs done.
    assert!(
        kill_pane_process(&project_id, &session_name),
        "kill -9 of the pane process must succeed"
    );

    let events_path = project.join(".cosmon/state/events.jsonl");

    // (a) WorkerExited first.
    let _ev = wait_for_worker_exited(&events_path, &mol_id, Duration::from_secs(5))
        .expect("WorkerExited must land before Harvested");

    // (c) Fleet entry for this molecule must be purged within 15s —
    // cs done does merge + teardown and can be slow under heavy CI.
    let fleet_path = project.join(".cosmon/state/fleet.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut still_registered = true;
    while Instant::now() < deadline && still_registered {
        still_registered = fleet_contains_molecule(&fleet_path, &mol_id);
        if still_registered {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    assert!(
        !still_registered,
        "fleet.json still has a worker bound to {mol_id} — \
         harvest-triggered `cs done` must purge it"
    );

    let _ = Command::new("tmux")
        .args(["-L", &project_id, "kill-server"])
        .output();
}

fn fleet_contains_molecule(fleet_path: &Path, mol_id: &str) -> bool {
    let Ok(s) = fs::read_to_string(fleet_path) else {
        return false;
    };
    let Ok(fleet) = serde_json::from_str::<serde_json::Value>(&s) else {
        return false;
    };
    let Some(workers) = fleet.get("workers").and_then(|v| v.as_object()) else {
        return false;
    };
    workers
        .values()
        .any(|w| w.get("current_molecule").and_then(|v| v.as_str()) == Some(mol_id))
}
