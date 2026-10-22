// SPDX-License-Identifier: AGPL-3.0-only

//! Executable client contract over real disposable tmux servers; no model credit.

use cosmon_core::{id::WorkerId, process::MoleculeProcess, terminal::TerminalIdentity};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore};
use std::process::Command;

struct Fixture {
    root: tempfile::TempDir,
    store: FileStore,
    molecule: MoleculeData,
    socket: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config_dir = root.path().join(".cosmon");
        std::fs::create_dir(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[project]\nproject_id = \"example-project\"\n",
        )
        .unwrap();
        let state = config_dir.join("state");
        let store = FileStore::new(&state);
        store.save_fleet(&Fleet::default()).unwrap();
        let now = chrono::Utc::now();
        let molecule: MoleculeData = serde_json::from_value(serde_json::json!({
            "id": "task-20990101-a001", "formula_id": "example", "status": "running",
            "variables": {}, "created_at": now, "updated_at": now,
            "total_steps": 1, "current_step": 0, "completed_steps": [], "links": []
        }))
        .unwrap();
        store.save_molecule(&molecule.id, &molecule).unwrap();
        let socket = format!(
            "cosmon-client-test-{}-{}",
            std::process::id(),
            root.path().file_name().unwrap().to_string_lossy()
        );
        Self {
            root,
            store,
            molecule,
            socket,
        }
    }

    fn launch(&mut self) {
        std::fs::write(self.root.path().join("worker.py"), "import os\nprint('WORKER-READONLY-WITNESS', flush=True)\nwhile True:\n data=os.read(0,4096)\n if not data: break\n with open('worker-input', 'ab') as f: f.write(data)\n").unwrap();
        let identity = cosmon_transport::TmuxBackend::new(&self.socket)
            .spawn_worker_with_env_witnessed(
                "fixture",
                self.root.path().to_str().unwrap(),
                "exec python3 worker.py",
                &[],
            )
            .unwrap()
            .expect("supported host must witness the real launch");
        let mut process = MoleculeProcess::new(WorkerId::new("fixture-worker").unwrap(), "fixture");
        process.terminal = Some(identity);
        self.molecule.bind_process(process);
        self.store
            .save_molecule(&self.molecule.id, &self.molecule)
            .unwrap();
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(self.root.path()).args([
            "--config",
            self.root.path().join(".cosmon/state").to_str().unwrap(),
            "--json",
            "terminal",
        ]);
        cmd
    }

    fn view(&self) -> serde_json::Value {
        let output = self
            .command()
            .arg(self.molecule.id.as_str())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

#[test]
fn missing_and_legacy_bindings_are_distinct_and_never_attach_by_label() {
    let mut f = Fixture::new();
    assert_eq!(f.view()["reason"], "no_process");
    let process = MoleculeProcess::new(WorkerId::new("fixture-worker").unwrap(), "plausible-label");
    f.molecule.bind_process(process);
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    let before = std::fs::read(f.store.molecule_dir(&f.molecule.id).join("state.json")).unwrap();
    let view = f.view();
    assert_eq!(view["schema_version"], 1);
    assert_eq!(view["kind"], "cosmon.terminal");
    assert_eq!(view["availability"], "unknown");
    assert_eq!(view["reason"], "identity_unknown");
    assert!(view.get("expected").is_none());
    let output = f
        .command()
        .args([f.molecule.id.as_str(), "--attach", "--expect", "stale"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(f.store.molecule_dir(&f.molecule.id).join("state.json")).unwrap(),
        before
    );
    assert!(!f
        .command()
        .arg("task-20990101")
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn real_tmux_launch_rejects_stale_token_reused_name_and_kernel_fingerprint() {
    let mut f = Fixture::new();
    f.launch();
    let view = f.view();
    assert_eq!(view["availability"], "available");
    assert_eq!(view["capability"], "read_only_ignore_size");
    assert_eq!(
        view["project_root"],
        f.root.path().canonicalize().unwrap().to_str().unwrap()
    );
    let stale = f
        .command()
        .args([f.molecule.id.as_str(), "--attach", "--expect", "sha256:bad"])
        .output()
        .unwrap();
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("token changed"));
    if let Some(TerminalIdentity::Tmux { pane_start, .. }) =
        f.molecule.process.as_mut().unwrap().terminal.as_mut()
    {
        *pane_start += 1;
    }
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    assert_eq!(f.view()["availability"], "unavailable");
    assert_eq!(f.view()["reason"], "identity_changed");
    // A replacement at the SAME label cannot inherit the launch witness.
    let backend = cosmon_transport::TmuxBackend::new(&f.socket);
    let neighbor = backend
        .spawn_worker_with_env_witnessed(
            "neighbor",
            f.root.path().to_str().unwrap(),
            "sleep 180",
            &[],
        )
        .unwrap();
    assert!(neighbor.is_some());
    assert!(Command::new("tmux")
        .args(["-L", &f.socket, "kill-session", "-t", "=fixture"])
        .status()
        .unwrap()
        .success());
    backend
        .spawn_worker("fixture", f.root.path().to_str().unwrap(), "sleep 180")
        .unwrap();
    assert_ne!(f.view()["availability"], "available");
}

#[test]
#[ignore = "explicit real Herdr/PTY acceptance; creates only owned disposable resources"]
fn owned_herdr_client_acceptance() {
    owned_client_acceptance("herdr-companion");
}

#[test]
#[ignore = "explicit real cmux/PTY acceptance; creates only owned disposable workspaces"]
fn owned_cmux_client_acceptance() {
    owned_client_acceptance("cmux-companion");
}

#[test]
#[ignore = "explicit real Pi/TUI acceptance; requires a pinned Pi test installation"]
fn owned_pi_client_acceptance() {
    owned_client_acceptance("pi-companion");
}

#[test]
fn optional_pi_client_contract() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/pi-companion/test_client.mjs");
    match Command::new("node").arg("--test").arg(script).status() {
        Ok(status) => assert!(status.success(), "Pi public client contract tests failed"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP optional Pi tooling tests: Node.js is unavailable");
        }
        Err(error) => panic!("could not run Pi client contract tests: {error}"),
    }
}

fn owned_client_acceptance(companion: &str) {
    let mut f = Fixture::new();
    f.launch();
    // The ensemble contract scopes listings to this configured project.
    f.molecule.project_id = Some(cosmon_core::id::ProjectId::new("example-project").unwrap());
    // Existing branch mapping for worktree navigation; no Herdr worktree creation.
    for argv in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(Command::new("git")
            .current_dir(f.root.path())
            .args(argv)
            .status()
            .unwrap()
            .success());
    }
    let branch = Command::new("git")
        .current_dir(f.root.path())
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .unwrap();
    f.molecule.originating_branch =
        Some(String::from_utf8(branch.stdout).unwrap().trim().to_owned());
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    std::fs::write(
        f.store.molecule_dir(&f.molecule.id).join("synthesis.md"),
        "owned artifact witness\n",
    )
    .unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../tools/{companion}/test_real_client.py"));
    assert!(Command::new("python3")
        .arg(script)
        .args([
            "--cs",
            env!("CARGO_BIN_EXE_cs"),
            "--repo",
            f.root.path().to_str().unwrap(),
            "--tmux-socket",
            &f.socket
        ])
        .status()
        .unwrap()
        .success());
}

#[test]
fn explicit_interactive_io_resize_and_stale_refusal() {
    let mut f = Fixture::new();
    f.launch();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/herdr-companion/test_interactive.py");
    assert!(Command::new("python3")
        .arg(script)
        .args([
            "--cs",
            env!("CARGO_BIN_EXE_cs"),
            "--repo",
            f.root.path().to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
}

#[test]
fn terminal_witness_stamp_cannot_rebind_a_replaced_dispatch() {
    let mut f = Fixture::new();
    f.launch();
    let expected = f.molecule.process.clone().unwrap();
    let identity = expected.terminal.as_ref().unwrap();
    f.molecule.bind_process(MoleculeProcess::new(
        WorkerId::new("replacement-worker").unwrap(),
        "fixture",
    ));
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    let path = f.store.molecule_dir(&f.molecule.id).join("state.json");
    let before = std::fs::read(&path).unwrap();
    assert!(!cosmon_runtime::dispatch_ledger::stamp_terminal_witness(
        &f.store,
        &f.molecule.id,
        Some(&expected),
        identity
    ));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn wrong_project_and_multiple_panes_refuse_the_capability() {
    let mut f = Fixture::new();
    f.launch();
    f.molecule.project_id = Some(cosmon_core::id::ProjectId::new("other-project").unwrap());
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    assert!(!f
        .command()
        .arg(f.molecule.id.as_str())
        .output()
        .unwrap()
        .status
        .success());
    f.molecule.project_id = None;
    f.store.save_molecule(&f.molecule.id, &f.molecule).unwrap();
    let TerminalIdentity::Tmux { pane_id, .. } = f
        .molecule
        .process
        .as_ref()
        .unwrap()
        .terminal
        .as_ref()
        .unwrap()
    else {
        panic!("expected tmux test fixture");
    };
    assert!(Command::new("tmux")
        .args([
            "-L",
            &f.socket,
            "split-window",
            "-d",
            "-t",
            &format!("%{pane_id}"),
            "sleep 180"
        ])
        .status()
        .unwrap()
        .success());
    assert_eq!(f.view()["reason"], "ambiguous_terminal");
}
