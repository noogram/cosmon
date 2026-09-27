// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test — `cs wait` after `cs whisper` on a completed molecule
//! blocks until the worker has answered the whisper.
//!
//! The scenario is the one measured on the iteration loop: a worker finishes,
//! its molecule reads `completed`, its pane is still alive; the pilot reviews
//! the branch, whispers a correction, and runs `cs wait`. Before the fix that
//! `cs wait` returned in zero seconds with zero transitions, because the
//! molecule already sat in the terminal set — the pilot had no signal that the
//! correction had landed and had to poll `git log` by hand.
//!
//! The worker here is a `bash` loop in a real tmux pane: it reads the pasted
//! whisper, sleeps, and commits on `feat/<id>`. `bash` is admitted through the
//! documented `[whisper] allowed_commands` override, so the pane-signature
//! gate runs unchanged. Driving tmux directly rather than through
//! `cs tackle` keeps the unit under test to whisper + wait.
//!
//! `DoD` assertions:
//!
//! 1. `cs wait` after the whisper exits 0 **after** the correction commit —
//!    not in zero seconds, and never before the branch has moved.
//! 2. Without any whisper, `cs wait` on the same completed molecule still
//!    returns at once (the `cs tackle M && cs wait M && cs done M` shape).
//!
//! `#[ignore]`'d because it spawns a real tmux session. Run with:
//!
//! ```bash
//! ./scripts/no-pilot-env.sh cargo test -p cosmon-cli --test wait_after_whisper -- --ignored
//! ```

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Seconds the fake worker spends "working" between reading the whisper and
/// committing its correction. Long enough that a zero-second return is
/// unambiguous, short enough to keep the test cheap.
const WORKER_TURN_SECS: u64 = 4;

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("TMUX_PANE")
        .env_remove("TMUX");
    cmd
}

fn run_ok(cmd: &mut Command, what: &str) -> Output {
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run_ok(
        Command::new("git").args(args).current_dir(dir),
        &format!("git {args:?}"),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn tmux(socket: &str, args: &[&str]) -> Output {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .expect("tmux")
}

/// Kills the fixture's tmux server however the test ends.
struct TmuxServer(String);

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = tmux(&self.0, &["kill-server"]);
    }
}

/// `cs init` a fresh repository and admit `bash` as a whisperable pane.
/// Returns `(tempdir guard, project root, tmux socket name)`.
fn init_fixture() -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("fixture");
    fs::create_dir_all(&project).unwrap();

    run_ok(
        cosmon_bin().args(["init", "--yes"]).current_dir(&project),
        "cs init",
    );

    let config_path = project.join(".cosmon/config.toml");
    let mut config: toml::Value =
        toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let socket = config["project"]["project_id"]
        .as_str()
        .expect("project_id")
        .to_owned();
    let table = config.as_table_mut().expect("config is a table");
    let whisper = table
        .entry("whisper")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    whisper
        .as_table_mut()
        .expect("[whisper] is a table")
        .insert(
            "allowed_commands".to_owned(),
            toml::Value::Array(vec![toml::Value::String("bash".to_owned())]),
        );
    fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "test@test.local"]);
    git(&project, &["config", "user.name", "cosmon-test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init"]);

    (tmp, project, socket)
}

/// Nucleate a molecule, then drive it to `completed` — the state C4 measured
/// `cs wait` returning from in zero seconds.
fn completed_molecule(project: &Path) -> String {
    let out = run_ok(
        cosmon_bin()
            .args(["--json", "nucleate", "task-work", "--no-parent", "--var"])
            .arg("topic=wait after whisper fixture")
            .current_dir(project),
        "cs nucleate",
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = parsed["id"].as_str().expect("nucleate id").to_owned();
    run_ok(
        cosmon_bin()
            .args(["complete", &mol_id, "--reason", "fixture"])
            .current_dir(project),
        "cs complete",
    );
    mol_id
}

/// The session name `cs whisper` resolves for this molecule.
fn session_name(project: &Path, mol_id: &str) -> String {
    let state = project
        .join(".cosmon/state/fleets/default/molecules")
        .join(mol_id)
        .join("state.json");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(state).unwrap()).unwrap();
    v["session_name"]
        .as_str()
        .map_or_else(|| mol_id.to_owned(), str::to_owned)
}

/// Run `cs wait` and return `(exit code, wall-clock, stdout+stderr)`.
fn cs_wait(project: &Path, mol_id: &str, timeout: u64) -> (i32, Duration, String) {
    let started = Instant::now();
    let out = cosmon_bin()
        .args([
            "wait",
            mol_id,
            "--timeout",
            &timeout.to_string(),
            "--poll-interval",
            "1",
        ])
        .current_dir(project)
        .output()
        .expect("cs wait");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), started.elapsed(), text)
}

#[test]
#[ignore = "requires tmux; run with `cargo test -- --ignored`"]
fn wait_after_whisper_on_completed_molecule_blocks_until_the_correction_commit() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let (tmp, project, socket) = init_fixture();
    let _server = TmuxServer(socket.clone());
    let mol_id = completed_molecule(&project);

    // DoD 2 first: no whisper, the trinity's `cs wait` is unchanged.
    let (code, took, text) = cs_wait(&project, &mol_id, 30);
    assert_eq!(code, 0, "cs wait without a whisper must succeed:\n{text}");
    assert!(
        took < Duration::from_secs(5),
        "cs wait without a whisper must return at once, took {took:?}:\n{text}"
    );

    // The worker's branch and worktree, as `cs tackle` would leave them.
    let branch = format!("feat/{mol_id}");
    let worktree = tmp.path().join("worktree");
    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            worktree.to_str().unwrap(),
        ],
    );
    let head_before = git(&project, &["rev-parse", &branch]);

    // The fake worker: wait for the pasted whisper, "work", commit.
    let session = session_name(&project, &mol_id);
    let script = format!(
        "read -r line; sleep {WORKER_TURN_SECS}; \
         git commit -q --allow-empty -m correction; \
         while true; do read -r _; done"
    );
    let out = tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            &session,
            "-c",
            worktree.to_str().unwrap(),
            "bash",
            "-c",
            &script,
        ],
    );
    assert!(
        out.status.success(),
        "tmux new-session failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::thread::sleep(Duration::from_millis(500));

    run_ok(
        cosmon_bin()
            .args(["whisper", &mol_id, "-m", "please add the missing line"])
            .current_dir(&project),
        "cs whisper",
    );

    // DoD 1: the wait covers the worker's turn.
    let (code, took, text) = cs_wait(&project, &mol_id, 60);
    let head_after = git(&project, &["rev-parse", &branch]);
    assert_eq!(code, 0, "cs wait after the whisper must succeed:\n{text}");
    assert_ne!(
        head_after, head_before,
        "cs wait returned before the correction commit landed on {branch} \
         (took {took:?}):\n{text}"
    );
    assert!(
        took >= Duration::from_secs(WORKER_TURN_SECS - 1),
        "cs wait after a whisper returned in {took:?}, before the worker's \
         {WORKER_TURN_SECS}s turn could end:\n{text}"
    );
}
