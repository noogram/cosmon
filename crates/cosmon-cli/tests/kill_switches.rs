// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #108 — the global stand-down switch stops every autonomous `cs`
//! path, and each scoped switch stops the component it names.
//!
//! Each test runs the real `cs` binary with `HOME` pointed at a tempdir, so
//! the kill-switch files live in `<tmp>/home/.cosmon/` and never touch the
//! operator's real home. A control test runs the same fixture with no switch
//! present and asserts the action *does* happen; without it, a "nothing
//! changed" assertion could pass because the fixture was never able to act.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway galaxy: a flat state dir, a `config.toml` carrying a project
/// identity (so `cs patrol` accepts it), a formulas dir, and a fake `$HOME`.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state_dir: PathBuf,
    formulas_dir: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let state_dir = root.join("state");
        let formulas_dir = root.join("formulas");
        let home = root.join("home");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::create_dir_all(home.join(".cosmon")).unwrap();
        fs::write(
            root.join("config.toml"),
            "[project]\nproject_id = \"kstest-a1b2\"\n",
        )
        .unwrap();
        fs::write(
            formulas_dir.join("ks-test.formula.toml"),
            r#"
formula = "ks-test"
version = 1
description = "kill-switch fixture"
id_prefix = "ks"

[[steps]]
id = "do"
title = "Do"
description = "Work"
acceptance = "Done"
"#,
        )
        .unwrap();
        Self {
            _tmp: tmp,
            root,
            state_dir,
            formulas_dir,
            home,
        }
    }

    /// Lay down `~/.cosmon/<name>` in the fake home.
    fn touch_switch(&self, name: &str) {
        fs::write(self.home.join(".cosmon").join(name), "").unwrap();
    }

    /// A `cs` command isolated from the ambient session and from the real
    /// `$HOME`.
    fn cs(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env_remove("COSMON_STATE_DIR")
            .env_remove("COSMON_CONFIG")
            .env_remove("COSMON_CONFIG_HOME")
            .env_remove("COSMON_FORMULAS_DIR")
            .env_remove("COSMON_ARTIFACT_DIR")
            .env_remove("COSMON_BASE_BRANCH")
            .env("COSMON_ASSUME_TRUSTED", "1");
        cmd
    }

    /// Nucleate a molecule whose TTL has already elapsed under a `collapse`
    /// expiry policy — the thing `cs patrol --expire` acts on.
    fn nucleate_expired(&self) -> String {
        let out = self
            .cs()
            .args(["--json", "nucleate", "ks-test"])
            .args(["--ttl", "7d", "--expiry-policy", "collapse"])
            .arg("--store-dir")
            .arg(&self.state_dir)
            .arg("--formulas-dir")
            .arg(&self.formulas_dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "nucleate: {}", stderr(&out));
        let j: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        let id = j["id"].as_str().unwrap().to_owned();
        let path = self.state_path(&id);
        let mut state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        state["expires_at"] = serde_json::json!("2000-01-01T00:00:00Z");
        fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
        id
    }

    fn state_path(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("fleets/default/molecules")
            .join(id)
            .join("state.json")
    }

    fn status_of(&self, id: &str) -> String {
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.state_path(id)).unwrap()).unwrap();
        state["status"].as_str().unwrap_or_default().to_owned()
    }

    fn patrol(&self, flags: &[&str]) -> Output {
        self.cs()
            .arg("--json")
            .arg("--config")
            .arg(&self.state_dir)
            .arg("patrol")
            .args(["--no-tmux"])
            .args(flags)
            .output()
            .unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn control_patrol_expire_collapses_an_expired_molecule() {
    let fx = Fixture::new();
    let id = fx.nucleate_expired();
    let out = fx.patrol(&["--expire"]);
    assert!(out.status.success(), "patrol: {}", stderr(&out));
    assert_eq!(fx.status_of(&id), "collapsed", "stdout: {}", stdout(&out));
}

#[test]
fn stand_down_stops_patrol_remediation() {
    let fx = Fixture::new();
    let id = fx.nucleate_expired();
    fx.touch_switch("stand-down.lock");
    let out = fx.patrol(&["--expire"]);
    assert!(out.status.success(), "patrol: {}", stderr(&out));
    assert_ne!(
        fx.status_of(&id),
        "collapsed",
        "stand-down.lock present, yet `cs patrol --expire` collapsed a molecule; stdout: {}",
        stdout(&out)
    );
    let j: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(j["stood_down"]["switch"], "stand-down.lock", "stdout: {j}");
}

#[test]
fn stand_down_stops_patrol_heal() {
    let fx = Fixture::new();
    fx.touch_switch("stand-down.lock");
    let out = fx.patrol(&["--heal", "--dry-run"]);
    assert!(out.status.success(), "patrol: {}", stderr(&out));
    let j: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(
        j["stood_down"]["switch"], "stand-down.lock",
        "stand-down.lock present, yet the heal pass ran; stdout: {j}"
    );
}

#[test]
fn health_off_still_stops_heal_but_not_the_rest_of_patrol() {
    let fx = Fixture::new();
    let id = fx.nucleate_expired();
    fx.touch_switch("health.off");
    let out = fx.patrol(&["--heal", "--dry-run", "--expire"]);
    assert!(out.status.success(), "patrol: {}", stderr(&out));
    let j: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(j["heal"]["kill_switched"], true, "stdout: {j}");
    // health.off is scoped: the expire sweep is not a heal action.
    assert_eq!(fx.status_of(&id), "collapsed", "stdout: {j}");
}

/// `cs ask --execute` shells out to `cs` on `PATH`. A fake `cs` records every
/// call in a marker file, so a dispatch attempt is observable without
/// creating a real molecule anywhere.
fn ask_fixture(fx: &Fixture) -> (PathBuf, PathBuf) {
    let galaxy = fx.root.join("mailroom");
    fs::create_dir_all(&galaxy).unwrap();
    let registry = fx.root.join("galaxies.toml");
    fs::write(
        &registry,
        format!(
            "[[galaxy]]\nname = \"mailroom\"\npath = \"{}\"\ndefault_formulas = {{ issue = \"task-work\" }}\n",
            galaxy.display()
        ),
    )
    .unwrap();
    let bin = fx.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = fx.root.join("cs-called");
    let fake = bin.join("cs");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho '{{\"id\":\"task-20260101-0000\"}}'\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (registry, marker)
}

fn run_ask(fx: &Fixture, registry: &Path) -> Output {
    let path = format!(
        "{}:{}",
        fx.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    fx.cs()
        .env("PATH", path)
        .arg("--config")
        .arg(&fx.state_dir)
        .args(["ask", "--experimental", "--execute", "--accept-default"])
        .arg("--registry")
        .arg(registry)
        .arg("fix the bug in mailroom")
        .output()
        .unwrap()
}

#[test]
fn control_ask_execute_dispatches() {
    let fx = Fixture::new();
    let (registry, marker) = ask_fixture(&fx);
    let out = run_ask(&fx, &registry);
    assert!(out.status.success(), "ask: {}", stderr(&out));
    assert!(
        marker.exists(),
        "control: ask never dispatched; {}",
        stdout(&out)
    );
}

#[test]
fn ask_off_stops_ask_dispatch() {
    let fx = Fixture::new();
    let (registry, marker) = ask_fixture(&fx);
    fx.touch_switch("ask.off");
    let out = run_ask(&fx, &registry);
    assert!(out.status.success(), "ask: {}", stderr(&out));
    assert!(
        !marker.exists(),
        "ask.off present, yet `cs ask --execute` shelled out to: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn stand_down_stops_ask_dispatch() {
    let fx = Fixture::new();
    let (registry, marker) = ask_fixture(&fx);
    fx.touch_switch("stand-down.lock");
    let out = run_ask(&fx, &registry);
    assert!(out.status.success(), "ask: {}", stderr(&out));
    assert!(
        !marker.exists(),
        "stand-down.lock present, yet `cs ask --execute` shelled out to: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn status_json_lists_active_kill_switches() {
    let fx = Fixture::new();
    fx.touch_switch("stand-down.lock");
    fx.touch_switch("ask.off");
    let out = fx
        .cs()
        .arg("--json")
        .arg("--config")
        .arg(&fx.state_dir)
        .arg("status")
        .output()
        .unwrap();
    assert!(out.status.success(), "status: {}", stderr(&out));
    let j: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    let active: Vec<&str> = j["kill_switches"]
        .as_array()
        .unwrap_or_else(|| panic!("no kill_switches array in: {j}"))
        .iter()
        .filter(|s| s["active"] == true)
        .filter_map(|s| s["file"].as_str())
        .collect();
    assert_eq!(active, ["stand-down.lock", "ask.off"], "stdout: {j}");
}
