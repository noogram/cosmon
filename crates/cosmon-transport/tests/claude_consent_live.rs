// SPDX-License-Identifier: AGPL-3.0-only

//! The acceptance test for the startup-consent pre-grant, run against the
//! **installed** Claude Code binary.
//!
//! # Why one green dispatch proves nothing
//!
//! `cosmon_transport::claude_trust` claims that a worker never meets a startup
//! dialog. Whether that holds is a property of the binary on the box, not of
//! our code, so no hermetic test can pin it — the unit tests pin the *shape* of
//! what we write, and this pins that Claude Code still honours it.
//!
//! The subtle half is the *second* spawn. Claude Code rewrites `.claude.json`
//! wholesale from its own in-memory state when a session ends, dropping keys
//! the running build does not recognise; which keys survive is version- and
//! state-dependent (measured on 2.1.220: `theme` and
//! `bypassPermissionsModeAccepted` stripped, `hasCompletedOnboarding` and the
//! trust key kept — while the issue-#20 tester measured the onboarding key
//! going to `null` on his bench, same mechanism, opposite outcome).
//!
//! So the pre-grant is an assertion re-made before every spawn, not an install
//! step — and the only test that can tell those two designs apart runs **two
//! consecutive spawns on one pristine config directory with nothing in
//! between**. A run-once design passes spawn 1 and fails spawn 2. That is
//! exactly the trap the tester documented, so it is the criterion.
//!
//! # Running it
//!
//! Ignored by default: it needs `tmux` and a real `claude` on `PATH`, takes
//! ~40s, and is a statement about the machine rather than about the commit.
//!
//! ```bash
//! cargo test -p cosmon-transport --test claude_consent_live -- --ignored --nocapture
//! ```
//!
//! No credential is needed or read. A pristine `CLAUDE_CONFIG_DIR` is
//! unauthenticated by construction (Claude Code derives its keychain service
//! name from the config-dir path), so the pane this test grades is a composer
//! carrying the `Not logged in · Run /login` footer — which is the proof
//! sought: reaching a composer at all means no dialog stood in front of it.
//!
//! Re-run after every Claude Code upgrade. A failure here means Claude Code
//! moved the keys and `claude_trust` needs re-measuring; the module docs record
//! the method.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Marker words that identify each screen a spawn can land on.
///
/// Grouped rather than inlined so a failure message can say *which* door the
/// worker stopped at instead of "not a composer".
mod screens {
    /// The first-run onboarding wizard — gate 0, the 2.1.220 report.
    pub const WIZARD: [&str; 2] = ["Choose the text style", "Let's get started"];
    /// The login-method selector, onboarding's second screen.
    pub const SELECTOR: &str = "Select login method";
    /// The folder-trust dialog — gate 1.
    pub const TRUST: [&str; 2] = ["Quick safety check", "Yes, I trust this folder"];
    /// The bypass-permissions disclaimer — gate 2.
    pub const DISCLAIMER: [&str; 2] = ["Bypass Permissions mode", "Yes, I accept"];
    /// Composer evidence: the footer Claude Code paints under its input box.
    /// Any one of these means the pane is accepting work.
    pub const COMPOSER: [&str; 3] = ["shift+tab to cycle", "bypass permissions on", "/login"];
}

/// Run a command and return its stdout, or `None` when it could not be run.
fn output_of(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `true` when `program` answers its version flag.
///
/// The flag is a parameter because `tmux` answers `-V` and *fails* on
/// `--version` — probing it with the wrong flag makes this test skip itself on
/// a machine that could have run it, which is the silent-green failure mode
/// this whole file exists to avoid. Both tools are preconditions the test skips
/// on rather than fails on: their absence says nothing about the commit.
fn available(program: &str, version_flag: &str) -> bool {
    Command::new(program)
        .arg(version_flag)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// What one spawn saw.
struct Spawn {
    /// The captured pane, verbatim — printed on failure so the reader grades
    /// the screen rather than our summary of it.
    pane: String,
}

impl Spawn {
    /// The first named door on the pane, or `None` for a composer.
    fn blocking_door(&self) -> Option<&'static str> {
        let hit = |needles: &[&str]| needles.iter().any(|n| self.pane.contains(n));
        if hit(&screens::WIZARD) {
            Some("the first-run onboarding wizard")
        } else if self.pane.contains(screens::SELECTOR) {
            Some("the login-method selector")
        } else if hit(&screens::TRUST) {
            Some("the folder-trust dialog")
        } else if hit(&screens::DISCLAIMER) {
            Some("the bypass-permissions disclaimer")
        } else if hit(&screens::COMPOSER) {
            None
        } else {
            Some("an unrecognised screen")
        }
    }
}

/// Pre-grant consent through the real production path, spawn `claude` in a
/// detached tmux session, let it settle, capture the pane, and tear the session
/// down.
///
/// The pre-grant is deliberately called *here*, once per spawn, because that is
/// the contract under test: `cs tackle` calls it on every dispatch.
fn spawn_once(socket: &str, config_dir: &Path, workspace: &Path, label: &str) -> Spawn {
    let paths =
        cosmon_transport::claude_trust::consent_paths(Some(&config_dir.to_string_lossy()), |k| {
            std::env::var(k).ok()
        })
        .expect("consent paths resolve");
    let outcome = cosmon_transport::claude_trust::pregrant_startup_consent(&paths, workspace)
        .unwrap_or_else(|e| panic!("{label}: pre-grant refused the spawn: {e}"));
    println!("{label}: pre-grant outcome = {outcome:?}");

    let session = format!("consent-{label}");
    let status = Command::new("tmux")
        .args(["-L", socket, "new-session", "-d", "-x", "200", "-y", "50"])
        .arg("-c")
        .arg(workspace)
        .arg("-s")
        .arg(&session)
        .arg(format!(
            "CLAUDE_CONFIG_DIR={} claude --permission-mode bypassPermissions",
            config_dir.display()
        ))
        .status()
        .expect("tmux new-session runs");
    assert!(
        status.success(),
        "{label}: tmux could not start the session"
    );

    // A cold Claude Code start paints its first frame in a few seconds; 20s is
    // the same settle the container bench uses, and an under-settled capture
    // would read as "unrecognised screen" rather than as the composer.
    std::thread::sleep(std::time::Duration::from_secs(20));
    let pane = output_of(
        "tmux",
        &["-L", socket, "capture-pane", "-p", "-t", &session],
    )
    .unwrap_or_default();

    // Quit the way an operator would, so the session ends by rewriting its
    // config — the event the second spawn has to survive. A bare kill would
    // skip the rewrite and make spawn 2 vacuous.
    for _ in 0..2 {
        let _ = Command::new("tmux")
            .args(["-L", socket, "send-keys", "-t", &session, "C-c"])
            .status();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-session", "-t", &session])
        .status();

    Spawn { pane }
}

/// Read the two granted `.claude.json` keys back, for the report.
fn granted_keys(config_dir: &Path, workspace: &Path) -> String {
    let raw = std::fs::read_to_string(config_dir.join(".claude.json")).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let ws = workspace.to_string_lossy().into_owned();
    format!(
        "hasCompletedOnboarding={} projects[ws].hasTrustDialogAccepted={}",
        json.get("hasCompletedOnboarding")
            .unwrap_or(&serde_json::Value::Null),
        json.get("projects")
            .and_then(|p| p.get(&ws))
            .and_then(|e| e.get("hasTrustDialogAccepted"))
            .unwrap_or(&serde_json::Value::Null),
    )
}

/// Canonicalize, because Claude Code keys trust on the path it resolves at
/// startup: on macOS a `/var/folders/…` tempdir is a symlink to
/// `/private/var/folders/…`, and a key written against the uncanonical form
/// silently fails to match — you then measure the trust dialog while believing
/// you measured the wizard.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalize {}: {e}", path.display()))
}

#[test]
#[ignore = "needs tmux + a real `claude` on PATH; ~40s; grades the installed binary, not this commit"]
fn two_consecutive_spawns_on_a_pristine_config_dir_never_meet_a_dialog() {
    // Named individually: "tmux and/or claude" would have hidden that the
    // probe, not the machine, was what was missing.
    let missing: Vec<&str> = [("tmux", "-V"), ("claude", "--version")]
        .into_iter()
        .filter(|(p, f)| !available(p, f))
        .map(|(p, _)| p)
        .collect();
    if !missing.is_empty() {
        println!("SKIP: not runnable on this machine — missing: {missing:?}");
        return;
    }
    println!(
        "claude version: {}",
        output_of("claude", &["--version"])
            .unwrap_or_default()
            .trim()
    );

    let root = tempfile::tempdir().expect("tempdir");
    let config_dir = canonical(root.path()).join("cfg");
    let workspace = canonical(root.path()).join("ws");
    std::fs::create_dir_all(&config_dir).expect("mkdir cfg");
    std::fs::create_dir_all(&workspace).expect("mkdir ws");
    let socket = "cosmon-consent-live";
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .status();

    // Spawn 1 — a config dir that has never seen Claude Code. Without the
    // onboarding pre-grant this is the theme wizard.
    let first = spawn_once(socket, &config_dir, &workspace, "spawn-1");
    println!("after spawn 1: {}", granted_keys(&config_dir, &workspace));

    // Spawn 2 — the same directory, now rewritten by spawn 1's exit. NOTHING
    // happens in between: no operator, no manual seed, only the pre-grant the
    // next dispatch makes for itself.
    let second = spawn_once(socket, &config_dir, &workspace, "spawn-2");
    println!("after spawn 2: {}", granted_keys(&config_dir, &workspace));

    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .status();

    for (label, spawn) in [("spawn 1", &first), ("spawn 2", &second)] {
        if let Some(door) = spawn.blocking_door() {
            panic!(
                "{label} stopped at {door} instead of reaching a composer.\n\
                 A worker here would hang with nobody to answer it.\n\
                 ── pane ──\n{}\n──────────",
                spawn.pane
            );
        }
        println!("{label}: reached the composer, no dialog");
    }
}
