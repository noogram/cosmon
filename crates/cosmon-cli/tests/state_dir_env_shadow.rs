// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #106 — `COSMON_STATE_DIR` silently outranks the galaxy found by
//! walk-up discovery from the current directory.
//!
//! # What the reporter observed
//!
//! `crates/cosmon-filestore/src/resolve.rs` lets `COSMON_STATE_DIR` win over
//! a galaxy found from cwd with no indication on stderr. The only existing
//! warning about state-dir resolution
//! (`crates/cosmon-cli/src/cmd/nucleate.rs`, `guard_global_fallback`) fires
//! only for the home-directory fallback and stays silent when the origin is
//! the environment variable. An operator running `cs` inside a tenant
//! galaxy on a host where `COSMON_STATE_DIR` points elsewhere reads and
//! writes the other galaxy's state, and `cs peek` then shows that other
//! fleet — which looks like the tenant's own fleet being hidden, not like a
//! misdirected read.
//!
//! # Decision
//!
//! `COSMON_STATE_DIR` keeps winning (an explicit ops override must stay
//! authoritative — see [`cosmon_filestore::resolve_state_dir_from`]'s own
//! doc comment), but the CLI now says so on stderr, naming both paths, the
//! same way `guard_global_fallback` already does for the global-fallback
//! case.
//!
//! This file pins the behaviour end to end through the real `cs` binary:
//! `cs status`, run from inside galaxy A with `COSMON_STATE_DIR` pointing at
//! galaxy B's state dir, reads galaxy B (RED without the fix would be: no
//! warning at all) and prints a warning naming both A and B.

use std::fs;
use std::path::Path;
use std::process::Command;

/// A `cs` invocation with the ambient cosmon session stripped, so the run is
/// hermetic — no inherited state-dir redirection from the session running
/// the test suite itself.
fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd);
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_STATE_DIR",
        "COSMON_CONFIG",
        "COSMON_CONFIG_HOME",
        "COSMON_FORMULAS_DIR",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

/// Initialize a bare galaxy (a `.cosmon/config.toml`-bearing root) at `root`.
fn init_galaxy(root: &Path) {
    fs::create_dir_all(root).unwrap();
    let out = cs(root).arg("init").output().expect("cs init should run");
    assert!(
        out.status.success(),
        "cs init failed at {}: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn env_state_dir_shadowing_project_warns_on_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy_a = tmp.path().join("galaxy-a");
    let galaxy_b = tmp.path().join("galaxy-b");
    init_galaxy(&galaxy_a);
    init_galaxy(&galaxy_b);

    let galaxy_b_state = galaxy_b.join(".cosmon").join("state");

    // Run `cs status` from inside galaxy A, but with COSMON_STATE_DIR
    // pointing at galaxy B's state dir — the exact shape from the issue.
    let out = cs(&galaxy_a)
        .env("COSMON_STATE_DIR", &galaxy_b_state)
        .arg("status")
        .output()
        .expect("cs status should run");

    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("COSMON_STATE_DIR"),
        "expected a warning naming the environment variable, got: {stderr}"
    );
    assert!(
        stderr.contains(galaxy_b_state.to_str().unwrap()),
        "expected the warning to name the env-resolved path {}, got: {stderr}",
        galaxy_b_state.display()
    );
    // The galaxy found by walk-up from cwd — the shadowed one — must be
    // named too, or an operator cannot tell which two fleets are in play.
    // The warning names the galaxy root (the unit a declaration binds to),
    // not its state directory, and prints the walk-up path canonicalized.
    let galaxy_a_root = galaxy_a.canonicalize().unwrap();
    assert!(
        stderr.contains(galaxy_a_root.to_str().unwrap()),
        "expected the warning to name the shadowed galaxy {}, got: {stderr}",
        galaxy_a_root.display()
    );
    assert!(
        stderr.contains("galaxy.toml"),
        "expected the warning to name the declaration remedy, got: {stderr}"
    );
}

/// A `galaxy.toml` declaration in the override state dir that records the
/// walk-up galaxy's `project_id` marks the override as intentional: no warning.
#[test]
fn declared_env_state_dir_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy_a = tmp.path().join("galaxy-a");
    let galaxy_b = tmp.path().join("galaxy-b");
    init_galaxy(&galaxy_a);
    init_galaxy(&galaxy_b);
    let galaxy_b_state = galaxy_b.join(".cosmon").join("state");
    let config = fs::read_to_string(galaxy_a.join(".cosmon/config.toml")).unwrap();
    let project_id = config
        .lines()
        .find_map(|l| l.trim().strip_prefix("project_id = \""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("init writes a project_id")
        .to_owned();
    fs::write(
        galaxy_b_state.join("galaxy.toml"),
        format!("project_id = \"{project_id}\"\n"),
    )
    .unwrap();

    let out = cs(&galaxy_a)
        .env("COSMON_STATE_DIR", &galaxy_b_state)
        .arg("status")
        .output()
        .expect("cs status should run");

    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("COSMON_STATE_DIR"),
        "a matching declaration must silence the warning, got: {stderr}"
    );
}

/// No galaxy in cwd or ancestors: `COSMON_STATE_DIR` is honoured with no
/// warning, matching the doc comment on
/// [`cosmon_filestore::resolve_state_dir_from`] — an explicit override
/// outside any galaxy is not a shadowing conflict.
#[test]
fn env_state_dir_outside_any_galaxy_is_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("not-a-galaxy");
    fs::create_dir_all(&outside).unwrap();
    let galaxy_b = tmp.path().join("galaxy-b");
    init_galaxy(&galaxy_b);
    let galaxy_b_state = galaxy_b.join(".cosmon").join("state");

    let out = cs(&outside)
        .env("COSMON_STATE_DIR", &galaxy_b_state)
        .arg("status")
        .output()
        .expect("cs status should run");

    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("COSMON_STATE_DIR"),
        "no galaxy in cwd — the env override should not be reported as shadowing anything, got: {stderr}"
    );
}
