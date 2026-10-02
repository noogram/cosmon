// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #155, item 1: `cs whisper` on a molecule whose recorded session does
//! not exist must say the session was not found.
//!
//! It used to report `pane_current_command=<missing>`, which reads as a pane
//! that exists and runs something unexpected. The rig renames nothing: it
//! keeps a live session under a *different* name on a private tmux socket and
//! points the molecule at the name that is absent, which is the shape a
//! renamed session has from the outside.

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

#[test]
fn whisper_to_a_missing_session_is_refused_as_session_not_found() {
    let socket = format!(
        "wh{}-{:04x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() & 0xffff)
    );
    let tmux = TmuxServer::new(&socket);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    git_ok(repo, &["init", "-q", "-b", "main"]);
    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        format!("[project]\nproject_id = \"{socket}\"\n"),
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

    let nuc = cs(repo)
        .args(["--json", "nucleate", "task-work", "--var", "topic=whisper"])
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "{}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().unwrap().to_owned();

    // The molecule records one name; a live session exists under another.
    let recorded = format!("recorded-{}", mol_id.rsplit('-').next().unwrap());
    let state_path = cosmon
        .join("state/fleets/default/molecules")
        .join(&mol_id)
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["session_name"] = recorded.clone().into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let started = tmux.run(&["new-session", "-d", "-s", "renamed-elsewhere", "sleep 300"]);
    assert!(started.status.success(), "tmux new-session failed");
    assert!(
        !tmux.has_session(&recorded),
        "precondition: recorded name is absent"
    );

    let out = cs(repo)
        .args(["whisper", &mol_id, "--message", "hello"])
        .output()
        .expect("cs whisper");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains("session not found") && stderr.contains(&recorded),
        "the refusal must say the session was not found, naming it; got: {stderr}"
    );
    assert!(
        !stderr.contains("<missing>"),
        "an absent session is not a pane with a missing command; got: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(6),
        "dedicated exit code; got: {stderr}"
    );
}
