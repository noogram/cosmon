// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end: a galaxy created by `cs init`, with nothing configured,
//! archives a terminal molecule *and* real git offers the result for
//! staging (issue #60).
//!
//! This is the falsifier the issue asks for, and it is deliberately not
//! two assertions about two subsystems: the defect was that each half was
//! pinned independently and their *agreement* was not. `[archive] enabled`
//! could default to `true` and the archive still never reach git; the
//! ignore body could be fixed and no project ever write a file for the
//! negation to act on. Only `git add -A -n` after a real terminal
//! transition observes both at once.
//!
//! `cs collapse` is the terminal transition used here, as in
//! `archive_wiring.rs`: `cs done` needs a git + tmux + worktree harness
//! that is prohibitively expensive and flaky in a test, and every archive
//! path it exercises is the same one `collapse` takes.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn git(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git")
}

#[test]
fn fresh_init_then_a_terminal_transition_leaves_stageable_archive_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    assert!(git(root, &["init", "-q", "."]).status.success());
    // A committer identity is needed only if we commit; `add -n` does not.

    // 1. `cs init` with no flags and no configuration of any kind.
    let init = cs()
        .arg("--json")
        .arg("init")
        .arg(root)
        .arg("--yes")
        .output()
        .expect("cs init");
    assert!(
        init.status.success(),
        "cs init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let cosmon_dir = root.join(".cosmon");
    let state_dir = cosmon_dir.join("state");
    let config_path = cosmon_dir.join("config.toml");
    assert!(
        !fs::read_to_string(&config_path)
            .expect("config.toml")
            .contains("[archive]"),
        "the point of this test is a galaxy that configures nothing"
    );

    let scoped = |args: &[&str]| -> std::process::Output {
        cs().env("COSMON_STATE_DIR", &state_dir)
            .env("COSMON_CONFIG", &config_path)
            .current_dir(&state_dir)
            .args(args)
            .output()
            .expect("cs")
    };

    // 2. Nucleate and collapse — a terminal transition on a trivial molecule.
    let nuc = scoped(&["--json", "nucleate", "task-work", "--var", "topic=issue 60"]);
    assert!(
        nuc.status.success(),
        "cs nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let nuc_json: serde_json::Value = serde_json::from_slice(&nuc.stdout).expect("json");
    let mol_id = nuc_json["id"].as_str().expect("id").to_owned();

    let col = scoped(&[
        "--json",
        "collapse",
        &mol_id,
        "--reason",
        "issue 60 falsifier",
    ]);
    assert!(
        col.status.success(),
        "cs collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );

    // 3. The archive exists on disk, without anyone opting in.
    let archive_root = state_dir.join("archive");
    assert!(
        archive_root.is_dir(),
        "a default galaxy must archive its terminal molecules"
    );

    // 4. …and real git offers those files for staging.
    let staged = String::from_utf8_lossy(&git(root, &["add", "-A", "-n"]).stdout).into_owned();
    let archived: Vec<&str> = staged
        .lines()
        .filter(|l| l.contains("state/archive/"))
        .collect();
    assert!(
        !archived.is_empty(),
        "git must offer the archive for staging; `git add -A -n` said:\n{staged}"
    );
    assert!(
        archived.iter().any(|l| l.contains(&mol_id)),
        "the collapsed molecule's own entry must be stageable, saw: {archived:?}"
    );

    // 5. …and ephemeral runtime state is still ignored, which is the half
    //    a naive `!state/**` would have broken.
    assert!(
        !staged.contains("state/fleet.json") && !staged.contains("/runtime.lock"),
        "ephemeral state must stay ignored; `git add -A -n` said:\n{staged}"
    );
}
