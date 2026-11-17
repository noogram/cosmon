// SPDX-License-Identifier: AGPL-3.0-only

//! Admission at `cs tackle` (issue #214, ADR-190 D2, increment B3).
//!
//! Real dispatches through the `cs` binary, with a stub `claude` on `PATH`
//! (no model call) and a private tmux server: with `max_active = 2` and two
//! live Running molecules the third spawn is refused before any worktree,
//! branch or session exists; the same third spawn is admitted once the cap is
//! raised, and an unreachable free-space floor refuses too.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn cs(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_DEFAULT_ADAPTER")
        .env("COSMON_CONFIG_HOME", cwd.join("isolated-config-home"));
    cmd
}

/// A `claude` that never talks to a model: it paints the composer line the
/// spawn postcondition looks for, then idles so the session stays alive.
fn install_stub_claude(dir: &Path, launched: &Path) -> std::path::PathBuf {
    let bin = dir.join("stub-bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in --version|-v) echo 'stub-claude 0.0.0'; exit 0;; esac\n\
         : > '{}'\n\
         stty -echo 2>/dev/null\n\
         printf '\\342\\235\\257 Type your message\\n'\n\
         exec sleep 600\n",
        launched.display()
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Number of tmux sessions on the test's private server (0 when it is down).
fn session_count(socket: &str) -> usize {
    let out = Command::new("tmux")
        .args(["-L", socket, "list-sessions", "-F", "#{session_name}"])
        .output()
        .unwrap();
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout).lines().count()
}

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn tackle_refuses_the_third_spawn_at_cap_two_before_any_worktree_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state_dir = root.join("state");
    let formulas_dir = root.join("formulas");
    let launched = root.join("launched");
    fs::create_dir_all(&formulas_dir).unwrap();
    let formula = "formula = \"adm\"\nversion = 1\ndescription = \"one step\"\n\
                   id_prefix = \"adm\"\n\n[[steps]]\nid = \"step-1\"\ntitle = \"Step 1\"\n\
                   description = \"work\"\nacceptance = \"done\"\n";
    fs::write(formulas_dir.join("adm.formula.toml"), formula).unwrap();

    let project_id = format!("admission-{}", std::process::id());
    let write_config = |admission: &str| {
        fs::create_dir_all(root.join(".cosmon")).unwrap();
        fs::write(
            root.join(".cosmon/config.toml"),
            format!("[project]\nproject_id = \"{project_id}\"\n{admission}"),
        )
        .unwrap();
    };
    write_config("[admission]\nmax_active = 2\n");
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@cosmon.invalid"][..],
        &["config", "user.name", "cosmon-test"][..],
        &["add", ".cosmon/config.toml"][..],
        &["commit", "-q", "-m", "test: initialize fixture"][..],
    ] {
        assert!(git(root, args).status.success());
    }

    let nucleate = || -> String {
        let out = cs(root)
            .args([
                "--json",
                "nucleate",
                "adm",
                "--store-dir",
                state_dir.to_str().unwrap(),
                "--formulas-dir",
                formulas_dir.to_str().unwrap(),
            ])
            .output()
            .expect("nucleate");
        assert!(out.status.success(), "{}", stderr(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["id"].as_str().unwrap().to_owned()
    };
    let ids = [nucleate(), nucleate(), nucleate()];

    let socket = project_id.clone();
    let stub_dir = install_stub_claude(root, &launched);
    let path = format!(
        "{}:{}",
        stub_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let tackle = |id: &str, extra: &[&str]| -> Output {
        cs(root)
            .env("PATH", &path)
            .env("COSMON_ALLOW_NO_WORKTREE", "1")
            .env(
                "CLAUDE_CODE_OAUTH_TOKEN",
                "test-placeholder-not-a-credential",
            )
            .args(["tackle", id, "--adapter", "claude", "--config"])
            .arg(&state_dir)
            .args(extra)
            .output()
            .expect("tackle")
    };
    let kill_server = || {
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    };

    for id in &ids[..2] {
        let out = tackle(id, &["--no-worktree"]);
        if !out.status.success() {
            kill_server();
            panic!("dispatch of {id} failed: {}", stderr(&out));
        }
    }
    let live = session_count(&socket);
    let third = &ids[2];
    let refused = tackle(third, &[]);
    let sessions_after_refusal = session_count(&socket);
    let worktree_exists = root.join(".worktrees").join(third).exists();
    let branch =
        String::from_utf8_lossy(&git(root, &["branch", "--list", &format!("feat/{third}")]).stdout)
            .into_owned();

    // The same spawn is admitted once the cap allows it: the refusal above
    // was the cap, not some other failure of this fixture.
    write_config("[admission]\nmax_active = 3\n");
    let admitted = tackle(third, &[]);
    let admitted_worktree = root.join(".worktrees").join(third).exists();
    kill_server();

    assert_eq!(live, 2, "fixture must hold two live sessions");
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    let err = stderr(&refused);
    assert!(err.contains("2 active molecules"), "{err}");
    assert!(err.contains("max_active is 2"), "{err}");
    assert!(
        err.contains("[admission].max_active"),
        "remedy missing: {err}"
    );
    assert_eq!(
        sessions_after_refusal, 2,
        "a refused spawn created a session"
    );
    assert!(!worktree_exists, "a refused spawn created a worktree");
    assert!(
        branch.trim().is_empty(),
        "a refused spawn created a branch: {branch}"
    );
    assert!(admitted.status.success(), "{}", stderr(&admitted));
    assert!(
        admitted_worktree,
        "the admitted spawn should have its worktree"
    );
}

#[test]
fn tackle_refuses_when_the_free_space_floor_is_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state_dir = root.join("state");
    let formulas_dir = root.join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    let formula = "formula = \"adm\"\nversion = 1\ndescription = \"one step\"\n\
                   id_prefix = \"adm\"\n\n[[steps]]\nid = \"step-1\"\ntitle = \"Step 1\"\n\
                   description = \"work\"\nacceptance = \"done\"\n";
    fs::write(formulas_dir.join("adm.formula.toml"), formula).unwrap();
    let project_id = format!("admission-floor-{}", std::process::id());
    fs::create_dir_all(root.join(".cosmon")).unwrap();
    fs::write(
        root.join(".cosmon/config.toml"),
        format!(
            "[project]\nproject_id = \"{project_id}\"\n[admission]\nmin_free_bytes = 9223372036854775807\n"
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
        assert!(git(root, args).status.success());
    }
    let out = cs(root)
        .args([
            "--json",
            "nucleate",
            "adm",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v["id"].as_str().unwrap().to_owned();

    let refused = cs(root)
        .env(
            "CLAUDE_CODE_OAUTH_TOKEN",
            "test-placeholder-not-a-credential",
        )
        .args(["tackle", &id, "--adapter", "claude", "--config"])
        .arg(&state_dir)
        .output()
        .unwrap();
    let err = stderr(&refused);
    assert_eq!(refused.status.code(), Some(1), "{err}");
    assert!(err.contains("bytes free"), "observed value missing: {err}");
    assert!(
        err.contains("min_free_bytes is 9223372036854775807"),
        "{err}"
    );
    assert!(!root.join(".worktrees").join(&id).exists());
    assert_eq!(session_count(&project_id), 0);
}
