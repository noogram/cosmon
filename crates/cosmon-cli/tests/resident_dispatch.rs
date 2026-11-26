// SPDX-License-Identifier: AGPL-3.0-only

//! Exercise the resident CLI's dispatch boundaries with disposable galaxies
//! and the spawn-matrix fake transport. No worker executable calls a model.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Galaxy {
    root: PathBuf,
    home: PathBuf,
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn success(out: Output) -> Output {
    assert!(out.status.success(), "{}", stderr(&out));
    out
}

impl Galaxy {
    fn new(home: &Path, name: &str, config: &str) -> Self {
        let root = home.join("galaxies").join(name);
        let cosmon = root.join(".cosmon");
        fs::create_dir_all(cosmon.join("formulas")).unwrap();
        fs::write(
            cosmon.join("config.toml"),
            format!("[project]\nproject_id = '{name}'\n{config}"),
        )
        .unwrap();
        fs::write(
            cosmon.join("formulas/resident.formula.toml"),
            "formula = 'resident'\nversion = 1\nid_prefix = 'task'\n\
             description = 'Resident test fixture'\n[[steps]]\nid = 'work'\n\
             title = 'Work'\ndescription = 'Synthetic input'\nacceptance = 'Observed'\n",
        )
        .unwrap();
        fs::write(root.join(".gitignore"), ".cosmon/state/\n.worktrees/\n").unwrap();

        let bins = home.join("bin");
        fs::create_dir_all(&bins).unwrap();
        let fake_tmux =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fakes/fake-tmux/tmux");
        write_executable(
            &bins.join("tmux"),
            &format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = -V ]; then echo 'tmux 3.4'; exit 0; fi\n\
                 if [ -n \"${{FIXTURE_DENY_PANE_LIST:-}}\" ] && [ \"$3\" = list-panes ]; then\n\
                   echo 'error connecting to fixture socket (Permission denied)' >&2; exit 1\n\
                 fi\n\
                 exec bash '{}' \"$@\"\n",
                fake_tmux.display()
            ),
        );
        for adapter in ["codex", "opencode"] {
            write_executable(
                &bins.join(adapter),
                &format!("#!/bin/sh\necho '{adapter} 0.0.0'\n"),
            );
        }

        let galaxy = Self {
            root,
            home: home.to_path_buf(),
        };
        galaxy.git(&["init", "-qb", "main"]);
        galaxy.git(&["config", "user.name", "Noogram"]);
        galaxy.git(&["config", "user.email", "test@example.org"]);
        galaxy.git(&["config", "commit.gpgsign", "false"]);
        galaxy.git(&["add", "."]);
        galaxy.git(&["commit", "-qm", "Initialize resident fixture"]);
        galaxy
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                    self.home.join("bin").display()
                ),
            )
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("COSMON_CONFIG_HOME", self.home.join("config"))
            .env("CODEX_HOME", self.home.join("codex"))
            .env("FAKE_TMUX_DIR", self.home.join("fake-tmux"))
            .env("FAKE_TMUX_INIT_OUTPUT", "opencode fixture ready")
            .env("LANG", "C.UTF-8");
        cmd
    }

    fn git(&self, args: &[&str]) -> Output {
        success(self.command("git").args(args).output().unwrap())
    }

    fn cs(&self, args: &[&str]) -> Command {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_cs"));
        cmd.args(args);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }

    fn nucleate(&self, extra: &[&str]) -> String {
        let out = success(
            self.cs(&["--json", "nucleate", "resident"])
                .args(extra)
                .output()
                .unwrap(),
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn resident(&self) -> Command {
        self.cs(&["run", "--resident", "_", "--timeout", "2"])
    }

    fn state(&self, id: &str) -> PathBuf {
        self.root
            .join(".cosmon/state/fleets/default/molecules")
            .join(id)
            .join("state.json")
    }

    fn observe(&self, id: &str) -> Value {
        let out = success(self.cs(&["--json", "observe", id]).output().unwrap());
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn trace_path(&self) -> PathBuf {
        self.root.join(".cosmon/state/runtime-trace.jsonl")
    }

    fn tackles(&self, id: &str) -> Vec<Value> {
        fs::read_to_string(self.trace_path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["action"] == "tackle" && row["molecule_id"] == id)
            .collect()
    }

    fn has_branch(&self, id: &str) -> bool {
        !self
            .git(&["branch", "--list", &format!("feat/{id}")])
            .stdout
            .is_empty()
    }

    fn has_worktree(&self, id: &str) -> bool {
        self.root.join(".worktrees").join(id).is_dir()
    }
}

#[test]
fn resident_records_a_failed_exec_launch_as_terminal_and_keeps_its_worktree() {
    for (fault, reason) in [
        (
            "FAKE_TMUX_NEW_SESSION_EXIT",
            "new-session failed (injected exit=1)",
        ),
        ("FAKE_TMUX_LIST_EMPTY", "last verdict=dead"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let galaxy = Galaxy::new(
            tmp.path(),
            "resident-launch",
            "[adapters.codex]\nmode = 'exec'\nupdate = 'skip'\n",
        );
        let id = galaxy.nucleate(&["--adapter", "codex"]);
        let run = galaxy.resident().env(fault, "1").output().unwrap();
        let attempts = galaxy.tackles(&id);
        assert!(
            attempts
                .iter()
                .any(|row| row["error"].as_str().is_some_and(|e| e.contains(reason))),
            "adapter must reach the failing spawn, fault={fault}, trace={attempts:?}, stderr={}",
            stderr(&run)
        );
        let observed = galaxy.observe(&id);
        assert_eq!(observed["status"], "collapsed", "{fault}: {observed:#}");
        assert!(observed["collapse_reason"]
            .as_str()
            .is_some_and(|e| e.contains(reason)));
        let state: Value = serde_json::from_slice(&fs::read(galaxy.state(&id)).unwrap()).unwrap();
        assert!(state["process"].is_null(), "{state:#}");
        let fleet: Value = serde_json::from_slice(
            &fs::read(galaxy.root.join(".cosmon/state/fleet.json")).unwrap(),
        )
        .unwrap();
        assert!(fleet["workers"].get(&id).is_none(), "{fleet:#}");
        assert!(galaxy.has_branch(&id));
        assert!(galaxy.has_worktree(&id));
        assert_eq!(attempts.len(), 1, "the failed dispatch must not retry");
    }
}

#[test]
fn resident_preserves_an_unresolved_exec_launch_if_the_worker_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy = Galaxy::new(
        tmp.path(),
        "resident-unresolved",
        "[adapters.codex]\nmode = 'exec'\nupdate = 'skip'\n",
    );
    let id = galaxy.nucleate(&["--adapter", "codex"]);
    // The fake reports a live pane, but has no kernel-backed terminal identity.
    let run = galaxy
        .resident()
        .env("FAKE_TMUX_INIT_OUTPUT", "OpenAI Codex fixture ready")
        .output()
        .unwrap();
    let attempts = galaxy.tackles(&id);
    assert_eq!(attempts.len(), 1, "{attempts:?}; {}", stderr(&run));
    let reason = attempts[0]["error"].as_str().unwrap();
    assert!(
        reason.contains("outcome unresolved") && reason.contains("creation identity"),
        "{reason}"
    );
    assert_eq!(galaxy.observe(&id)["status"], "running");
    assert!(galaxy.has_branch(&id));
    assert!(galaxy.has_worktree(&id));
}

#[test]
fn resident_does_not_record_worker_absence_when_the_socket_cannot_be_read() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy = Galaxy::new(
        tmp.path(),
        "resident-denied",
        "[adapters.codex]\nmode = 'exec'\nupdate = 'skip'\n",
    );
    let id = galaxy.nucleate(&["--adapter", "codex"]);
    let run = galaxy
        .resident()
        .env("FAKE_TMUX_NEW_SESSION_EXIT", "1")
        .env("FIXTURE_DENY_PANE_LIST", "1")
        .output()
        .unwrap();
    let attempts = galaxy.tackles(&id);
    assert_eq!(attempts.len(), 1, "{attempts:?}; {}", stderr(&run));
    let reason = attempts[0]["error"].as_str().unwrap();
    assert!(
        reason.contains("new-session failed") && reason.contains("outcome unresolved"),
        "{reason}"
    );
    assert_eq!(galaxy.observe(&id)["status"], "running");
}

#[test]
fn resident_checks_the_active_cap_between_dispatches_from_the_same_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let galaxy = Galaxy::new(tmp.path(), "resident-cap", "[admission]\nmax_active = 2\n");
    let mut ids: Vec<_> = (0..3)
        .map(|_| galaxy.nucleate(&["--adapter", "opencode"]))
        .collect();
    ids.sort();
    let run = galaxy.resident().output().unwrap();
    for id in &ids[..2] {
        assert_eq!(
            galaxy.observe(id)["status"],
            "running",
            "{:?}; {}",
            galaxy.tackles(id),
            stderr(&run)
        );
    }
    let third = &ids[2];
    let attempts = galaxy.tackles(third);
    assert!(
        attempts.iter().any(|row| row["error"]
            .as_str()
            .is_some_and(|e| e.contains("2 active molecules") && e.contains("max_active is 2"))),
        "{attempts:?}"
    );
    assert_eq!(galaxy.observe(third)["status"], "pending");
    assert!(!galaxy.has_branch(third));
    assert!(!galaxy.has_worktree(third));
}

#[test]
fn resident_holds_cross_galaxy_dependencies_until_landed_and_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let upstream = Galaxy::new(tmp.path(), "resident-upstream", "");
    let downstream = Galaxy::new(tmp.path(), "resident-downstream", "");
    let blocker = upstream.nucleate(&["--tag", "no-auto-harvest"]);
    success(
        upstream
            .cs(&["tackle", &blocker, "--adapter", "opencode"])
            .output()
            .unwrap(),
    );
    assert_eq!(upstream.observe(&blocker)["status"], "running");
    let edge = format!("resident-upstream:{blocker}");
    let child = downstream.nucleate(&["--blocked-by", &edge, "--adapter", "opencode"]);
    let unknown_edge = format!("resident-unknown:{blocker}");
    let unknown = downstream.nucleate(&[
        "--blocked-by",
        &edge,
        "--blocked-by",
        &unknown_edge,
        "--adapter",
        "opencode",
    ]);
    let deps = success(downstream.cs(&["deps", &child]).output().unwrap());
    assert!(String::from_utf8_lossy(&deps.stdout).contains("[cross-galaxy/running]"));

    let assert_held = |stage| {
        let run = downstream.resident().output().unwrap();
        assert_eq!(run.status.code(), Some(124), "{}", stderr(&run));
        for id in [&child, &unknown] {
            let attempts = downstream.tackles(id);
            assert!(
                attempts.is_empty(),
                "{stage}: {id} was dispatched: {attempts:#?}"
            );
            assert_eq!(downstream.observe(id)["status"], "pending");
            assert!(!downstream.has_worktree(id));
        }
    };
    assert_held("running");
    let direct = downstream
        .cs(&["tackle", &child, "--adapter", "opencode"])
        .output()
        .unwrap();
    assert!(
        !direct.status.success() && stderr(&direct).contains(&edge),
        "{}",
        stderr(&direct)
    );
    assert!(!downstream.has_worktree(&child));
    let worktree = upstream.root.join(".worktrees").join(&blocker);
    fs::write(worktree.join("result.txt"), "fixture result\n").unwrap();
    upstream.git(&["-C", worktree.to_str().unwrap(), "add", "result.txt"]);
    upstream.git(&[
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-qm",
        "Add fixture result",
    ]);
    success(
        upstream
            .cs(&["complete", &blocker, "--reason", "fixture completed"])
            .output()
            .unwrap(),
    );
    assert_eq!(upstream.observe(&blocker)["status"], "completed");
    assert_held("completed before landing");
    success(upstream.cs(&["done", &blocker]).output().unwrap());
    assert_eq!(
        fs::read_to_string(upstream.root.join("result.txt")).unwrap(),
        "fixture result\n"
    );

    let state = upstream.state(&blocker);
    let landed = fs::read(&state).unwrap();
    fs::write(&state, "{unreadable").unwrap();
    assert_held("unreadable");
    fs::write(&state, landed).unwrap();

    let run = downstream.resident().output().unwrap();
    assert_eq!(
        downstream.observe(&child)["status"],
        "running",
        "{}",
        stderr(&run)
    );
    let attempts = downstream.tackles(&child);
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert!(attempts[0]["error"].is_null(), "{attempts:?}");
    assert!(downstream.tackles(&unknown).is_empty());
}
