// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — `cs purge --sessions` reclaims a terminal molecule's
//! tmux session, and nothing else (ADR-179).
//!
//! The population this covers is the one no other surface can see. `cs purge`'s
//! sweep removes the fleet entry of a worker whose molecule went terminal and
//! leaves the session up; that entry was the only link back to the molecule, so
//! from then on the session is unattributable and accumulates for as long as
//! the machine is up. Measured on the development machine on 2026-09-22:
//! fifteen such panes, 4.0 GB resident, not one of them an idle shell.
//!
//! These tests drive tmux **directly** rather than through `cs tackle`. That is
//! deliberate on two counts. The unit under test is the reclaim pass, and a
//! fixture that spawns a whole agent to produce a session couples this file to
//! the entire dispatch path — which is, as of 2026-09-22, independently red in
//! this environment (`purge_stale_tmux` fails at the same `cs tackle` line with
//! `session died right after spawn`). A session created by `tmux new-session`
//! is the same object to every assertion below, and it costs a second rather
//! than ten minutes.
//!
//! `DoD` assertions:
//!
//! 1. A terminal molecule's detached session is killed.
//! 2. Its scrollback is captured into the molecule's directory **first** — the
//!    durable half is moved out of the way, not traded away.
//! 3. A non-terminal molecule's session on the same socket survives.
//! 4. A session no molecule claims survives: a tmux socket is a shared
//!    namespace, and absence is not ownership (ADR-179 §4).
//! 5. Without `--allow-unharvested` the pass is a dry register — it names the
//!    same session, kills nothing and writes nothing.
//!
//! `#[ignore]`'d because it spawns real tmux sessions. Run with:
//!
//! ```bash
//! cargo test -p cosmon-cli --test purge_sessions -- --ignored
//! ```

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // The pass refuses to reclaim the pane `cs` is running in. Under a
        // harness launched from inside tmux that pane is on another socket,
        // but the variable would still be consulted.
        .env_remove("TMUX_PANE")
        .env_remove("TMUX");
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

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .expect("tmux")
}

/// A detached session holding a process that does not exit — the shape of the
/// real leak, where the agent finishes its molecule and sits at its prompt.
fn spawn_session(socket: &str, name: &str) {
    let out = tmux(
        socket,
        &["new-session", "-d", "-s", name, "sh -c 'sleep 600'"],
    );
    assert!(
        out.status.success(),
        "tmux new-session {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn session_alive(socket: &str, name: &str) -> bool {
    tmux(socket, &["has-session", "-t", &format!("={name}")])
        .status
        .success()
}

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
    let socket = config["project"]["project_id"]
        .as_str()
        .expect("project_id")
        .to_owned();

    (tmp, project, socket)
}

/// Nucleate a molecule and return `(id, the session name cosmon would mint)`.
///
/// The name is computed with the production function, not re-derived here: a
/// test that reimplemented the slug would pass while the two drifted, which is
/// the exact failure the forward-computation rule exists to prevent.
fn nucleate(project: &Path, topic: &str) -> (String, String) {
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
    let session = cosmon_core::slugify::session_name_for(Some(topic), &mol_id);
    (mol_id, session)
}

/// Drive a molecule to a terminal status.
///
/// `Collapsed` rather than `Completed`: reaching `Completed` requires the
/// molecule to be `Running` first, which only a real dispatch produces, and
/// that is precisely the path these tests avoid. Both statuses are terminal,
/// the gate reads `is_terminal()` and nothing finer, and the core predicate
/// tests assert each of the two by name.
fn make_terminal(project: &Path, mol_id: &str) {
    let out = cosmon_bin()
        .args(["collapse", mol_id, "--reason", "fixture"])
        .current_dir(project)
        .output()
        .expect("cs collapse");
    assert!(
        out.status.success(),
        "cs collapse failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn molecule_dir(project: &Path, mol_id: &str) -> PathBuf {
    project
        .join(".cosmon/state/fleets/default/molecules")
        .join(mol_id)
}

fn purge_sessions(project: &Path, extra: &[&str]) -> String {
    let mut args = vec!["purge", "--sessions"];
    args.extend_from_slice(extra);
    let out = cosmon_bin()
        .args(&args)
        .current_dir(project)
        .output()
        .expect("cs purge --sessions");
    assert!(
        out.status.success(),
        "cs purge failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn purge_sessions_reclaims_only_a_terminal_molecules_session() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (_tmp, project, socket) = init_fixture();

    let (done_id, done_session) = nucleate(&project, "alpha beta gamma");
    let (_busy_id, busy_session) = nucleate(&project, "delta epsilon zeta");
    make_terminal(&project, &done_id);

    spawn_session(&socket, &done_session);
    spawn_session(&socket, &busy_session);
    // DoD 4: a session cosmon did not mint, on cosmon's own socket.
    spawn_session(&socket, "an-operators-own-scratch");

    let scrollback =
        molecule_dir(&project, &done_id).join(format!("session-scrollback-{done_session}.txt"));

    // --- DoD 5: without the executing gesture, the pass is a register. -----
    let dry = purge_sessions(&project, &["--dry-run"]);
    assert!(
        dry.contains(&done_session),
        "the dry register must name the reclaimable session; got:\n{dry}"
    );
    assert!(
        session_alive(&socket, &done_session),
        "a dry run must kill nothing"
    );
    assert!(
        !scrollback.exists(),
        "a dry run must write nothing, but {} exists",
        scrollback.display()
    );

    // --- DoD 1/2/3/4: execute. ---------------------------------------------
    let report = purge_sessions(&project, &["--allow-unharvested"]);

    assert!(
        !session_alive(&socket, &done_session),
        "a terminal molecule's session must be reclaimed; report:\n{report}"
    );
    assert!(
        session_alive(&socket, &busy_session),
        "a non-terminal molecule's session must survive; report:\n{report}"
    );
    assert!(
        session_alive(&socket, "an-operators-own-scratch"),
        "a session no molecule claims must survive — absence is not ownership; \
         report:\n{report}"
    );
    assert!(
        scrollback.exists(),
        "the scrollback must be captured before the kill, but {} is missing; \
         report:\n{report}",
        scrollback.display()
    );

    let _ = tmux(&socket, &["kill-server"]);
}

/// The case that makes the leak permanent: the sweep has already removed the
/// fleet entry, so the roster can no longer say which molecule this session
/// belonged to. Attribution must still succeed, because the session name is
/// computed forward from the molecule rather than parsed back out of the name.
#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn a_session_orphaned_by_the_sweep_is_still_attributable() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (_tmp, project, socket) = init_fixture();
    let (done_id, done_session) = nucleate(&project, "orphaned alpha beta");
    make_terminal(&project, &done_id);
    spawn_session(&socket, &done_session);

    // Empty the roster, exactly as the sweep's orphan branch leaves it: the
    // session now has nothing at all pointing at it.
    let fleet_path = project.join(".cosmon/state/fleet.json");
    if let Ok(raw) = fs::read_to_string(&fleet_path) {
        let mut fleet: serde_json::Value = serde_json::from_str(&raw).unwrap();
        if let Some(workers) = fleet.get_mut("workers").and_then(|v| v.as_object_mut()) {
            workers.clear();
        }
        fs::write(&fleet_path, serde_json::to_string_pretty(&fleet).unwrap()).unwrap();
    }

    let report = purge_sessions(&project, &["--allow-unharvested"]);

    assert!(
        report.contains(&done_id),
        "an orphaned session must still be attributed to its molecule; \
         report:\n{report}"
    );
    assert!(
        !session_alive(&socket, &done_session),
        "an orphaned session must be reclaimable; report:\n{report}"
    );

    let _ = tmux(&socket, &["kill-server"]);
}
