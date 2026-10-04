// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end wiring test: a real `cs tackle --adapter claude` writes the
//! presence hooks and the project's deny rules into the settings overlay the
//! worker is launched with (issue #166, follow-up to #163).
//!
//! The overlay builders (`write_settings_overlay_for_work`, `add_deny_rules`)
//! have unit tests, but those call the functions directly. This test proves
//! the call sites in `cmd/tackle.rs` exist and feed the launched worker: a
//! stub `claude` executable on `PATH` (no model call) records the `--settings`
//! path it was started with and a copy of the document found there, and the
//! assertions read that copy.
//!
//! The tmux server is private to the test (`COSMON_TMUX_SOCKET`) and killed
//! afterwards.

#![cfg(unix)]

use cosmon_core::copilot_hook::{HookEvent, HookProvider};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Deny rule configured in the scratch galaxy's `[worker].deny_rules`.
const DENY_RULE: &str = "Bash(git push:*)";

fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// A `claude` that never talks to a model: it records where it was told to
/// read settings from, snapshots that file, paints the composer line the
/// spawn postcondition looks for, then idles so the tmux session stays alive
/// until the test kills the server.
fn install_stub_claude(dir: &Path, capture: &Path) -> std::path::PathBuf {
    let bin = dir.join("stub-bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in --version|-v) echo 'stub-claude 0.0.0'; exit 0;; esac\n\
         prev=''\n\
         for a in \"$@\"; do\n\
           if [ \"$prev\" = '--settings' ]; then\n\
             printf '%s' \"$a\" > '{cap}/settings.path'\n\
             cp \"$a\" '{cap}/settings.json'\n\
           fi\n\
           prev=\"$a\"\n\
         done\n\
         : > '{cap}/launched'\n\
         stty -echo 2>/dev/null\n\
         printf '\\342\\235\\257 Type your message\\n'\n\
         exec sleep 600\n",
        cap = capture.display()
    );
    let path = bin.join("claude");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn wait_for(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn tackle_claude_launch_overlay_carries_presence_hooks_and_deny_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state_dir = root.join("state");
    let formulas_dir = root.join("formulas");
    let capture = root.join("capture");
    fs::create_dir_all(&formulas_dir).unwrap();
    fs::create_dir_all(&capture).unwrap();

    let formula = "formula = \"overlay-wiring\"\nversion = 1\ndescription = \"one step\"\n\
                   id_prefix = \"ow\"\n\n[[steps]]\nid = \"step-1\"\ntitle = \"Step 1\"\n\
                   description = \"work\"\nacceptance = \"done\"\n";
    fs::write(formulas_dir.join("overlay-wiring.formula.toml"), formula).unwrap();

    // The scratch galaxy: identity plus the deny rule under test. The tmux
    // socket is derived from `project_id`, so a per-process id keeps this
    // test's server private.
    let project_id = format!("overlay-wiring-{}", std::process::id());
    fs::create_dir_all(root.join(".cosmon")).unwrap();
    fs::write(
        root.join(".cosmon/config.toml"),
        format!(
            "[project]\nproject_id = \"{project_id}\"\n\n[worker]\ndeny_rules = [\"{DENY_RULE}\"]\n"
        ),
    )
    .unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@cosmon.invalid"][..],
        &["config", "user.name", "cosmon-test"][..],
        &["add", ".cosmon/config.toml"][..],
        &["commit", "-q", "-m", "test: initialize fixture"][..],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    let nucleated = cs(root)
        .args([
            "--json",
            "nucleate",
            "overlay-wiring",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate");
    assert!(
        nucleated.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nucleated.stderr)
    );
    let molecule: serde_json::Value = serde_json::from_slice(&nucleated.stdout).unwrap();
    let mol_id = molecule["id"].as_str().unwrap().to_owned();

    let socket = project_id.clone();
    let stub_dir = install_stub_claude(root, &capture);
    let path = format!(
        "{}:{}",
        stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let tackled = cs(root)
        .env("PATH", path)
        .env("COSMON_ALLOW_NO_WORKTREE", "1")
        // The stub claude never authenticates, but tackle refuses to spawn an
        // interactive worker without a credential. CI has no keychain item, so
        // give the preflight a placeholder token (never sent anywhere).
        .env(
            "CLAUDE_CODE_OAUTH_TOKEN",
            "test-placeholder-not-a-credential",
        )
        .args([
            "tackle",
            &mol_id,
            "--no-worktree",
            "--adapter",
            "claude",
            "--config",
            state_dir.to_str().unwrap(),
        ])
        .output()
        .expect("tackle");
    let launched = wait_for(&capture.join("launched"));
    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
    assert!(
        tackled.status.success(),
        "tackle failed: {}",
        String::from_utf8_lossy(&tackled.stderr)
    );
    assert!(launched, "the stub claude was never launched by tackle");
    assert!(
        capture.join("settings.json").exists(),
        "the worker was launched without a --settings overlay"
    );

    let overlay: serde_json::Value =
        serde_json::from_slice(&fs::read(capture.join("settings.json")).unwrap()).unwrap();

    // (a) presence hooks: one registration per accepted event, each running
    // `cs sessions hook run --event <event> --provider claude`.
    let hook_commands = |event: &str| -> Vec<String> {
        overlay["hooks"][event]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    for event in HookEvent::ACCEPTED {
        let name = event.provider_event(HookProvider::Claude).unwrap();
        let wanted = format!(
            "sessions hook run --event {} --provider claude",
            event.as_str()
        );
        assert!(
            hook_commands(name).iter().any(|c| c.contains(&wanted)),
            "overlay has no presence hook `{wanted}` under {name}: {overlay:#}"
        );
    }

    // (b) the configured deny rule reached the harness's own permissions.deny.
    let deny: Vec<&str> = overlay["permissions"]["deny"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(
        deny.contains(&DENY_RULE),
        "permissions.deny lacks {DENY_RULE:?}: {overlay:#}"
    );
}
