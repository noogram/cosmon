// SPDX-License-Identifier: AGPL-3.0-only

//! Actual `cs tackle` qualification with a fake Codex executable and isolated
//! tmux server. Like the existing tier-2 transport tests, this is run explicitly
//! on a host with tmux; the ordinary offline suite needs no terminal installation.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use cosmon_core::{
    id::MoleculeId,
    molecule::MoleculeStatus,
    worker_exit::{WorkerExitObservation, WorkerExitStore as _},
};
use cosmon_filestore::FileStore;
use cosmon_state::{event_log, worker_exit::FileWorkerExitStore, StateStore};

type TestResult = Result<(), Box<dyn Error>>;

struct Rig {
    root: tempfile::TempDir,
    sockets: tempfile::TempDir,
    bins: PathBuf,
}

impl Rig {
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.root.path())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                    self.bins.display()
                ),
            )
            .env("HOME", self.root.path())
            .env("LANG", "C.UTF-8")
            .env("COSMON_CONFIG_HOME", self.root.path().join("config-home"))
            .env("CODEX_HOME", self.root.path().join("codex-home"))
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("GIT_AUTHOR_NAME", "Noogram")
            .env("GIT_COMMITTER_NAME", "Noogram")
            .env("GIT_AUTHOR_EMAIL", "test@example.org")
            .env("GIT_COMMITTER_EMAIL", "test@example.org");
        command
    }

    fn cs(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        let result = self.command(env!("CARGO_BIN_EXE_cs")).args(args).output()?;
        if !result.status.success() {
            return Err(format!("cs {args:?}: {}", String::from_utf8_lossy(&result.stderr)).into());
        }
        Ok(result)
    }

    fn state(&self) -> PathBuf {
        self.root.path().join(".cosmon/state")
    }

    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let sockets = tempfile::Builder::new().prefix("cse-").tempdir_in("/tmp")?;
        let bins = root.path().join("bin");
        fs::create_dir(&bins)?;
        let rig = Self {
            root,
            sockets,
            bins,
        };
        let config = rig.root.path().join(".cosmon");
        fs::create_dir_all(config.join("formulas"))?;
        fs::write(config.join("config.toml"), "[project]\nproject_id='receipt-0000'\n[adapters.codex]\nmode='exec'\nupdate='skip'\nworker_exit_receipts=true\n")?;
        fs::write(config.join("formulas/witness.formula.toml"), "formula='witness'\nversion=1\nid_prefix='witness'\ndescription='No-model worker witness'\n[[steps]]\nid='observe'\ntitle='Observe'\ndescription='Synthetic task input only'\nacceptance='OS evidence recorded'\n")?;
        let executable = rig.bins.join("codex");
        fs::write(
            &executable,
            r#"#!/bin/sh
if [ "$1" = --version ]; then printf 'codex 0.0.0\n'; exit 0; fi
printf '%s\000' "$@" > "$COSMON_MOL_DIR/argv"
printf 'openai codex\nready\n'
while :; do
  if [ -f "$COSMON_MOL_DIR/control" ]; then
    read -r action value < "$COSMON_MOL_DIR/control"
    rm "$COSMON_MOL_DIR/control"
    case "$action" in
      exit) exit "$value";;
      signal) kill -"$value" "$$";;
    esac
  fi
  sleep 0.05
done
"#,
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        for args in [
            vec!["init", "-q"],
            vec!["add", ".cosmon"],
            vec!["commit", "-qm", "test: initialize isolated worker witness"],
        ] {
            let result = rig.command("git").args(args).output()?;
            if !result.status.success() {
                return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
            }
        }
        let result = rig
            .command("tmux")
            .args([
                "-L",
                "receipt-0000",
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "neighbor",
                "sleep 120",
            ])
            .output()?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
        }
        Ok(rig)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self
            .command("tmux")
            .args(["-L", "receipt-0000", "kill-server"])
            .output();
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn configured_exec_keeps_its_incarnation_after_configuration_and_namespace_change() -> TestResult {
    use cosmon_core::transport::TransportBackend as _;
    use cosmon_core::transport_selection::TransportSource;
    for receipts_enabled in [false, true] {
        for (index, source) in [
            TransportSource::Default,
            TransportSource::Global,
            TransportSource::Project,
        ]
        .into_iter()
        .enumerate()
        {
            let rig = Rig::new()?;
            let config_path = rig.root.path().join(".cosmon/config.toml");
            let base = fs::read_to_string(&config_path)?.replace(
                "worker_exit_receipts=true",
                &format!("worker_exit_receipts={receipts_enabled}"),
            );
            fs::write(&config_path, &base)?;
            let global = rig.root.path().join("config-home/cosmon");
            fs::create_dir_all(&global)?;
            let ledger = FileStore::new(rig.state());
            if index > 0 {
                fs::write(global.join("config.toml"), "[transport]\nbackend='tmux'\n")?;
            }
            if index == 2 {
                fs::write(
                    &config_path,
                    format!("{base}\n[transport]\nbackend='tmux'\n"),
                )?;
            }
            let value: serde_json::Value =
                serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
            let id = MoleculeId::new(value["id"].as_str().ok_or("no molecule")?)?;
            let session = format!("pinned-{index}");
            let config_before = fs::read_to_string(&config_path)?;
            let pending = fs::read(ledger.molecule_dir(&id).join("state.json"))?;
            for rejected in ["herdr", "zellij", "unknown-host"] {
                fs::write(
                    &config_path,
                    format!("{base}\n[transport]\nbackend='{rejected}'\n"),
                )?;
                let output = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .args(["tackle", id.as_str(), "--adapter", "codex"])
                    .output()?;
                assert!(!output.status.success());
                assert_eq!(
                    fs::read(ledger.molecule_dir(&id).join("state.json"))?,
                    pending
                );
                assert!(!rig
                    .root
                    .path()
                    .join(".worktrees")
                    .join(id.as_str())
                    .exists());
                assert!(!rig
                    .command("git")
                    .args([
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/feat/{id}")
                    ])
                    .status()?
                    .success());
                assert!(!rig
                    .command("tmux")
                    .args(["-L", "receipt-0000", "has-session", "-t", &session])
                    .output()?
                    .status
                    .success());
            }
            fs::write(&config_path, config_before)?;
            rig.cs(&[
                "tackle",
                id.as_str(),
                "--adapter",
                "codex",
                "--name",
                &session,
            ])?;
            let mol = ledger.load_molecule(&id)?;
            let process = mol.process.as_ref().ok_or("no process")?;
            assert_eq!(process.transport.ok_or("no selection")?.source, source);
            let terminal = process.terminal.as_ref().ok_or("no incarnation")?;
            let before = serde_json::to_value(&mol)?;
            // Both global and project now request unimplemented hosts. Existing
            // handles must remain usable without re-resolving either file.
            fs::write(
                &config_path,
                format!("{base}\n[transport]\nbackend='herdr'\n"),
            )?;
            fs::write(
                global.join("config.toml"),
                "[transport]\nbackend='zellij'\n",
            )?;
            let other = tempfile::Builder::new().prefix("csb-").tempdir_in("/tmp")?;
            let neighbor = rig
                .command("tmux")
                .env("TMUX_TMPDIR", other.path())
                .args([
                    "-L",
                    "receipt-0000",
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    &session,
                    "sleep 120",
                ])
                .output()?;
            assert!(neighbor.status.success());
            let result = (|| -> TestResult {
                let view = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args(["terminal", id.as_str(), "--json"])
                    .output()?;
                assert!(
                    view.status.success(),
                    "{}",
                    String::from_utf8_lossy(&view.stderr)
                );
                let view: serde_json::Value = serde_json::from_slice(&view.stdout)?;
                assert_eq!(view["identity"], serde_json::to_value(terminal)?);
                assert_eq!(view["availability"], "available");
                for command in [
                    vec!["freeze", session.as_str()],
                    vec!["thaw", session.as_str()],
                    vec!["resume", "--agent", session.as_str()],
                    vec!["tackle", id.as_str(), "--adapter", "codex", "--force"],
                ] {
                    assert!(!rig
                        .command(env!("CARGO_BIN_EXE_cs"))
                        .args(command)
                        .output()?
                        .status
                        .success());
                }
                assert_eq!(serde_json::to_value(ledger.load_molecule(&id)?)?, before);
                let pinned =
                    cosmon_transport::TmuxBackend::for_process(Some(process), "unrelated")?;
                assert!(pinned
                    .capture_output(&process.worker_id, 20)?
                    .to_lowercase()
                    .contains("codex"));
                // Losing a modern snapshot cannot turn the same worker label
                // into a legacy handle routed through current configuration.
                let mut lost = mol.clone();
                lost.process = None;
                ledger.save_molecule(&id, &lost)?;
                let lost_bytes = fs::read(ledger.molecule_dir(&id).join("state.json"))?;
                // The detached startup model observer is an independent writer.
                // Let its negative observation finish before asserting no control effects.
                assert!(wait_for(|| {
                    event_log::read_all(
                    rig.state().join("events.jsonl")
                )
                .is_ok_and(|events| events.iter().any(|e| matches!(
                    &e.event,
                    cosmon_core::event_v2::EventV2::ModelObservationUnavailable { mol_id, .. } if mol_id == &id
                )))
                }));
                let fleet_bytes = fs::read(rig.state().join("fleet.json"))?;
                let event_bytes = fs::read(rig.state().join("events.jsonl"))?;
                for command in [
                    vec!["freeze", session.as_str(), "--timeout", "0"],
                    vec!["thaw", session.as_str()],
                    vec!["resume", "--agent", session.as_str()],
                    vec!["whisper", id.as_str(), "--message", "resume"],
                ] {
                    let refused = rig
                        .command(env!("CARGO_BIN_EXE_cs"))
                        .env("TMUX_TMPDIR", other.path())
                        .args(&command)
                        .output()?;
                    assert!(!refused.status.success(), "unexpected success: {command:?}");
                    assert!(
                        String::from_utf8_lossy(&refused.stderr).contains("identity")
                            || String::from_utf8_lossy(&refused.stderr).contains("continuation"),
                        "wrong refusal: {command:?}: {}",
                        String::from_utf8_lossy(&refused.stderr)
                    );
                    assert_eq!(
                        fs::read(ledger.molecule_dir(&id).join("state.json"))?,
                        lost_bytes
                    );
                    assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_bytes);
                    assert_eq!(fs::read(rig.state().join("events.jsonl"))?, event_bytes);
                    assert!(
                        rig.command("tmux")
                            .env("TMUX_TMPDIR", other.path())
                            .args(["-L", "receipt-0000", "has-session", "-t", &session])
                            .status()?
                            .success(),
                        "same-name neighbor changed: {command:?}"
                    );
                }
                // Absence of the entire molecule snapshot is also positive unknown,
                // whereas genuinely old fleet-only records keep their legacy path.
                let state_path = ledger.molecule_dir(&id).join("state.json");
                let hidden_path = ledger.molecule_dir(&id).join("state.unavailable");
                fs::rename(&state_path, &hidden_path)?;
                let refused = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args(["freeze", &session, "--timeout", "0"])
                    .output()?;
                fs::rename(&hidden_path, &state_path)?;
                assert!(!refused.status.success());
                assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_bytes);
                assert_eq!(fs::read(rig.state().join("events.jsonl"))?, event_bytes);
                assert!(rig
                    .command("tmux")
                    .env("TMUX_TMPDIR", other.path())
                    .args(["-L", "receipt-0000", "has-session", "-t", &session])
                    .status()?
                    .success());
                let refused = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args(["purge", &session, "--force", "--allow-unharvested"])
                    .output()?;
                assert!(!refused.status.success());
                assert!(String::from_utf8_lossy(&refused.stderr).contains("identity"));
                assert_eq!(fs::read(&state_path)?, lost_bytes);
                assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_bytes);
                let audit = fs::read(rig.state().join("events.jsonl"))?;
                assert!(audit.starts_with(&event_bytes));
                let appended: Vec<serde_json::Value> = audit[event_bytes.len()..]
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(serde_json::from_slice)
                    .collect::<Result<_, _>>()?;
                assert_eq!(appended.len(), 1, "unexpected audit delta: {appended:?}");
                let appended = &appended[0];
                assert_eq!(appended["type"], "operator_signed");
                assert_eq!(appended["action"], "cs purge");
                assert_eq!(appended["signature_method"], "shell");
                assert!(rig
                    .command("tmux")
                    .env("TMUX_TMPDIR", other.path())
                    .args(["-L", "receipt-0000", "has-session", "-t", &session])
                    .status()?
                    .success());
                // Reusing the worker label for a different legacy molecule must
                // preserve the old molecule's positive hosting evidence.
                let legacy_id = MoleculeId::new(format!("reused-20261019-{index:04x}"))?;
                let mut legacy = lost.clone();
                legacy.id = legacy_id.clone();
                ledger.save_molecule(&legacy_id, &legacy)?;
                event_log::emit_one(
                    rig.state().join("events.jsonl"),
                    cosmon_core::event_v2::EventV2::WorkerSpawned {
                        transport: None,
                        worker_id: process.worker_id.clone(),
                        molecule: Some(legacy_id.clone()),
                        session_name: session.clone(),
                        role: "polecat".into(),
                        adapter_name: "codex".into(),
                        loop_ownership: Default::default(),
                    },
                    None,
                )?;
                let reuse_events = fs::read(rig.state().join("events.jsonl"))?;
                let config = fs::read(&config_path)?;
                let mut allowed = config.clone();
                allowed.extend_from_slice(b"\n[whisper]\nallowed_commands=['sleep']\n");
                fs::write(&config_path, allowed)?;
                let old_refused = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args(["whisper", id.as_str(), "--message", "probe", "--dry-run"])
                    .output()?;
                assert!(!old_refused.status.success());
                assert!(String::from_utf8_lossy(&old_refused.stderr).contains("identity"));
                assert_eq!(fs::read(&state_path)?, lost_bytes);
                assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_bytes);
                assert_eq!(fs::read(rig.state().join("events.jsonl"))?, reuse_events);
                let legacy_allowed = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args([
                        "whisper",
                        legacy_id.as_str(),
                        "--message",
                        "probe",
                        "--dry-run",
                    ])
                    .output()?;
                fs::write(&config_path, config)?;
                assert!(
                    legacy_allowed.status.success(),
                    "{}",
                    String::from_utf8_lossy(&legacy_allowed.stderr)
                );
                ledger.save_molecule(&id, &mol)?;
                // The real control effect must kill A while same-name B survives.
                let stopped = rig
                    .command(env!("CARGO_BIN_EXE_cs"))
                    .env("TMUX_TMPDIR", other.path())
                    .args(["purge", &session, "--force", "--allow-unharvested"])
                    .output()?;
                assert!(
                    stopped.status.success(),
                    "{}",
                    String::from_utf8_lossy(&stopped.stderr)
                );
                assert!(rig
                    .command("tmux")
                    .env("TMUX_TMPDIR", other.path())
                    .args(["-L", "receipt-0000", "has-session", "-t", &session])
                    .status()?
                    .success());
                Ok(())
            })();
            let _ = rig
                .command("tmux")
                .env("TMUX_TMPDIR", other.path())
                .args(["-L", "receipt-0000", "kill-server"])
                .output();
            result?;
            fs::write(&config_path, &base)?;
            let _ = fs::remove_file(global.join("config.toml"));
        }
    }
    Ok(())
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn tackle_wait_receipts_and_harvest_preserve_os_results_without_completing_work() -> TestResult {
    let rig = Rig::new()?;
    let ledger = FileStore::new(rig.state());
    for (index, control, expected) in [
        (0, "exit 0", WorkerExitObservation::Exited { code: 0 }),
        (1, "exit 7", WorkerExitObservation::Exited { code: 7 }),
        (2, "exit 143", WorkerExitObservation::Exited { code: 143 }),
        (
            3,
            "signal 15",
            WorkerExitObservation::Signaled {
                signal: 15,
                core_dumped: false,
            },
        ),
        (
            4,
            "signal 9",
            WorkerExitObservation::Signaled {
                signal: 9,
                core_dumped: false,
            },
        ),
    ] {
        let value: serde_json::Value =
            serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
        let id = MoleculeId::new(value["id"].as_str().ok_or("nucleate returned no id")?)?;
        rig.cs(&[
            "tackle",
            id.as_str(),
            "--adapter",
            "codex",
            "--name",
            &format!("worker-{index}"),
        ])?;
        let directory = ledger.molecule_dir(&id);
        let receipts =
            FileWorkerExitStore::current(&directory)?.ok_or("missing prepared launch")?;
        let process = ledger
            .load_molecule(&id)?
            .process
            .ok_or("missing process")?;
        let terminal = process.terminal.ok_or("missing exact terminal identity")?;
        let birth: serde_json::Value =
            serde_json::from_slice(&fs::read(receipts.directory().join("birth.json"))?)?;
        let cosmon_core::terminal::TerminalIdentity::Tmux { pane_pid, .. } = &terminal;
        assert_eq!(birth["supervisor_pid"].as_u64(), Some(u64::from(*pane_pid)));
        let view: serde_json::Value =
            serde_json::from_slice(&rig.cs(&["--json", "terminal", id.as_str()])?.stdout)?;
        assert_eq!(view["availability"], "available");
        assert_eq!(view["capability"], "read_only_ignore_size");
        assert_eq!(view["identity"], serde_json::to_value(&terminal)?);
        assert!(view["expected"]
            .as_str()
            .is_some_and(|v| v.starts_with("sha256:")));
        let identity = serde_json::to_string(&receipts.request().identity)?;
        fs::write(directory.join("control"), control)?;
        assert!(wait_for(|| receipts.read() == expected));
        assert_eq!(ledger.load_molecule(&id)?.status, MoleculeStatus::Running);
        for _ in 0..2 {
            rig.cs(&[
                "harvest",
                "--molecule",
                id.as_str(),
                "--from-pane-died",
                "--exit-code",
                "0",
                "--worker-launch",
                &identity,
                "--dry-run",
            ])?;
        }
        let observations=event_log::read_all(rig.state().join("events.jsonl"))?.into_iter().filter(|envelope| matches!(&envelope.event, cosmon_core::event_v2::EventV2::WorkerExitObserved {molecule_id,observation,..} if molecule_id==&id && observation==&expected)).count();
        assert_eq!(observations, 1, "hook replay must not emit twice");
        assert_eq!(ledger.load_molecule(&id)?.status, MoleculeStatus::Running);
        rig.cs(&[
            "complete",
            id.as_str(),
            "--reason",
            "Explicit test completion, independent of OS exit",
        ])?;
        fs::rename(
            receipts.directory().join("result.json"),
            receipts.directory().join("held-result.json"),
        )?;
        let output = rig.cs(&["--json", "harvest", "--molecule", id.as_str(), "--dry-run"])?;
        assert!(String::from_utf8_lossy(&output.stdout).contains("receipt_unknown"));
        fs::rename(
            receipts.directory().join("held-result.json"),
            receipts.directory().join("result.json"),
        )?;
        let output = rig.cs(&["--json", "harvest", "--molecule", id.as_str(), "--dry-run"])?;
        assert!(String::from_utf8_lossy(&output.stdout).contains("dry_run"));
    }
    let neighbor = rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", "=neighbor"])
        .output()?;
    assert!(neighbor.status.success());
    Ok(())
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn interactive_receipt_opt_in_is_refused_before_dispatch_effects() -> TestResult {
    let rig = Rig::new()?;
    let value: serde_json::Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
    let id = MoleculeId::new(value["id"].as_str().ok_or("missing molecule id")?)?;
    let ledger = FileStore::new(rig.state());
    let directory = ledger.molecule_dir(&id);
    let state_before = fs::read(directory.join("state.json"))?;
    let refs_before = rig.command("git").args(["show-ref"]).output()?;
    let worktrees_before = rig
        .command("git")
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    let panes_before = rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "list-panes",
            "-a",
            "-F",
            "#{session_id}:#{pane_id}:#{pane_pid}",
        ])
        .output()?;
    assert!(
        refs_before.status.success()
            && worktrees_before.status.success()
            && panes_before.status.success()
    );
    for mode in [Some("interactive"), None] {
        let mode_line = mode
            .map(|mode| format!("mode='{mode}'\n"))
            .unwrap_or_default();
        fs::write(rig.root.path().join(".cosmon/config.toml"), format!("[project]\nproject_id='receipt-0000'\n[adapters.codex]\n{mode_line}update='skip'\nworker_exit_receipts=true\n"))?;
        let output = rig
            .command(env!("CARGO_BIN_EXE_cs"))
            .args(["tackle", id.as_str(), "--adapter", "codex"])
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires mode = 'exec'"));
        assert_eq!(fs::read(directory.join("state.json"))?, state_before);
        assert!(ledger.load_molecule(&id)?.process.is_none());
        for path in [
            directory.join("worker-launch.json"),
            directory.join("worker-launches"),
            directory.join("argv"),
            rig.root.path().join(".worktrees"),
        ] {
            assert!(
                !path.exists(),
                "refused dispatch created {}",
                path.display()
            );
        }
        assert_eq!(
            rig.command("git").args(["show-ref"]).output()?.stdout,
            refs_before.stdout
        );
        assert_eq!(
            rig.command("git")
                .args(["worktree", "list", "--porcelain"])
                .output()?
                .stdout,
            worktrees_before.stdout
        );
        assert_eq!(
            rig.command("tmux")
                .args([
                    "-L",
                    "receipt-0000",
                    "list-panes",
                    "-a",
                    "-F",
                    "#{session_id}:#{pane_id}:#{pane_pid}"
                ])
                .output()?
                .stdout,
            panes_before.stdout
        );
    }
    Ok(())
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn harvesting_an_unbound_molecule_never_adopts_a_same_name_neighbor() -> TestResult {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, WorkerId};
    use cosmon_core::worker::WorkerStatus;

    let rig = Rig::new()?;
    let value: serde_json::Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
    let id = MoleculeId::new(value["id"].as_str().ok_or("no molecule")?)?;
    rig.cs(&["collapse", id.as_str(), "--reason", "unbound qualification"])?;
    let ledger = FileStore::new(rig.state());
    let mut fleet = ledger.load_fleet()?;
    let worker_id = WorkerId::new(id.as_str())?;
    fleet.workers.insert(
        worker_id.clone(),
        cosmon_state::WorkerData::new(
            worker_id.clone(),
            AgentId::new("unrelated")?,
            AgentRole::Implementation,
            Clearance::Execute,
            WorkerStatus::Active,
        ),
    );
    ledger.save_fleet(&fleet)?;
    let before = fs::read(rig.state().join("fleet.json"))?;
    assert!(rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "new-session",
            "-d",
            "-s",
            id.as_str(),
            "sleep 120"
        ])
        .status()?
        .success());
    fs::rename(
        rig.state().join("events.jsonl"),
        rig.state().join("events.saved"),
    )?;
    fs::create_dir(rig.state().join("events.jsonl"))?;
    let result = rig
        .command(env!("CARGO_BIN_EXE_cs"))
        .args([
            "done",
            id.as_str(),
            "--no-merge",
            "--no-worktree-remove",
            "--no-branch-delete",
            "--no-auto-propel",
        ])
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(rig.state().join("fleet.json"))?, before);
    assert!(ledger.load_fleet()?.workers.contains_key(&worker_id));
    assert!(rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", id.as_str()])
        .status()?
        .success());
    // The older fleet-only representation remains usable without fabricating
    // a molecule-id target. Its explicit worker label differs from the neighbor.
    fs::remove_dir(rig.state().join("events.jsonl"))?;
    fs::rename(
        rig.state().join("events.saved"),
        rig.state().join("events.jsonl"),
    )?;
    let legacy_id = WorkerId::new("legacy-seat")?;
    let mut legacy = cosmon_state::WorkerData::new(
        legacy_id.clone(),
        AgentId::new("legacy")?,
        AgentRole::Implementation,
        Clearance::Execute,
        WorkerStatus::Active,
    );
    legacy.current_molecule = Some(id.clone());
    let mut fleet = ledger.load_fleet()?;
    fleet.workers.insert(legacy_id.clone(), legacy);
    ledger.save_fleet(&fleet)?;
    assert!(rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "new-session",
            "-d",
            "-s",
            legacy_id.as_str(),
            "sleep 120"
        ])
        .status()?
        .success());
    rig.cs(&[
        "done",
        id.as_str(),
        "--no-merge",
        "--no-worktree-remove",
        "--no-branch-delete",
        "--no-auto-propel",
    ])?;
    assert!(!rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "has-session",
            "-t",
            legacy_id.as_str()
        ])
        .status()?
        .success());
    assert!(rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", id.as_str()])
        .status()?
        .success());
    let fleet = ledger.load_fleet()?;
    assert!(!fleet.workers.contains_key(&legacy_id));
    assert!(fleet.workers.contains_key(&worker_id));
    // Preserve the older, explicit tackle fleet convention too. This marker
    // is established legacy state, distinct from an unrelated same-name seat.
    let mut fleet = fleet;
    fleet
        .workers
        .get_mut(&worker_id)
        .ok_or("neighbor missing")?
        .agent_id = AgentId::new("tackle")?;
    ledger.save_fleet(&fleet)?;
    rig.cs(&[
        "done",
        id.as_str(),
        "--no-merge",
        "--no-worktree-remove",
        "--no-branch-delete",
        "--no-auto-propel",
    ])?;
    assert!(!rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", id.as_str()])
        .status()?
        .success());
    assert!(!ledger.load_fleet()?.workers.contains_key(&worker_id));
    Ok(())
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn force_purge_pinned_server_gone_preserves_replacement() -> TestResult {
    force_purge_stale_pin(true)
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn force_purge_pinned_live_server_preserves_replacement() -> TestResult {
    force_purge_stale_pin(false)
}

fn force_purge_stale_pin(restart_server: bool) -> TestResult {
    use cosmon_core::terminal::TerminalIdentity;
    let rig = Rig::new()?;
    let config = rig.root.path().join(".cosmon/config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)?
            .replace("worker_exit_receipts=true", "worker_exit_receipts=false"),
    )?;
    let value: serde_json::Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
    let id = MoleculeId::new(value["id"].as_str().ok_or("no molecule")?)?;
    let session = "stale-pin";
    rig.cs(&[
        "tackle",
        id.as_str(),
        "--adapter",
        "codex",
        "--name",
        session,
    ])?;
    let ledger = FileStore::new(rig.state());
    let molecule = ledger.load_molecule(&id)?;
    let process = molecule.process.as_ref().ok_or("no process")?;
    let TerminalIdentity::Tmux {
        server_pid,
        server_start,
        socket_path,
        ..
    } = process.terminal.as_ref().ok_or("no terminal")?;
    assert_eq!(
        cosmon_process_witness::process_start_time(*server_pid),
        Some(*server_start)
    );
    // End the dummy executable before replacing its terminal, avoiding an orphan.
    fs::write(ledger.molecule_dir(&id).join("control"), "exit 0\n")?;
    assert!(wait_for(|| !ledger
        .molecule_dir(&id)
        .join("control")
        .exists()));
    let removal = if restart_server {
        rig.command("tmux")
            .args(["-S", socket_path, "kill-server"])
            .output()?
    } else {
        rig.command("tmux")
            .args(["-S", socket_path, "kill-session", "-t", session])
            .output()?
    };
    assert!(
        removal.status.success(),
        "{}",
        String::from_utf8_lossy(&removal.stderr)
    );
    if restart_server {
        assert!(wait_for(|| cosmon_process_witness::process_start_time(
            *server_pid
        ) != Some(*server_start)));
    }
    let replacement = rig
        .command("tmux")
        .args([
            "-S",
            socket_path,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            session,
            "sleep 120",
        ])
        .output()?;
    assert!(
        replacement.status.success(),
        "{}",
        String::from_utf8_lossy(&replacement.stderr)
    );
    if !restart_server {
        assert_eq!(
            cosmon_process_witness::process_start_time(*server_pid),
            Some(*server_start)
        );
    }
    let replacement_identity = || -> Result<Vec<u8>, Box<dyn Error>> {
        let out = rig
            .command("tmux")
            .args([
                "-S",
                socket_path,
                "display-message",
                "-p",
                "-t",
                session,
                "#{pid}:#{session_id}:#{session_created}:#{pane_id}:#{pane_pid}",
            ])
            .output()?;
        assert!(out.status.success(), "replacement must remain alive");
        Ok(out.stdout)
    };
    let before = replacement_identity()?;
    // load_molecule derives process liveness; compare persisted bytes instead
    // to assert that reclamation itself never rewrites the molecule.
    let molecule_path = ledger.molecule_dir(&id).join("state.json");
    let molecule_before_purge = fs::read(&molecule_path)?;
    let purged = rig.cs(&["--json", "purge", session, "--force", "--allow-unharvested"])?;
    let purged: serde_json::Value = serde_json::from_slice(&purged.stdout)?;
    assert_eq!(purged["tmux_killed"], false);
    let message = purged["terminal_cleanup"]
        .as_str()
        .ok_or("missing cleanup reason")?;
    if restart_server {
        assert!(
            message.contains("recorded tmux server is gone"),
            "{message}"
        );
    } else {
        assert!(
            message.contains("incarnation changed or unavailable"),
            "{message}"
        );
    }
    assert!(!ledger
        .load_fleet()?
        .workers
        .contains_key(&process.worker_id));
    assert_eq!(fs::read(&molecule_path)?, molecule_before_purge);
    assert_eq!(
        replacement_identity()?,
        before,
        "purge must never kill the same-name replacement"
    );
    Ok(())
}

#[test]
#[ignore = "real tmux qualification: run explicitly with --ignored on a POSIX host"]
fn force_purge_pinned_transport_error_retains_entry_then_exact_purge_kills() -> TestResult {
    use cosmon_core::terminal::TerminalIdentity;
    let rig = Rig::new()?;
    let value: serde_json::Value =
        serde_json::from_slice(&rig.cs(&["--json", "nucleate", "witness"])?.stdout)?;
    let id = MoleculeId::new(value["id"].as_str().ok_or("no molecule")?)?;
    let session = "unavailable-pin";
    rig.cs(&[
        "tackle",
        id.as_str(),
        "--adapter",
        "codex",
        "--name",
        session,
    ])?;
    let ledger = FileStore::new(rig.state());
    let original = ledger.load_molecule(&id)?;
    let mut unavailable = original.clone();
    let TerminalIdentity::Tmux { socket_path, .. } = unavailable
        .process
        .as_mut()
        .and_then(|process| process.terminal.as_mut())
        .ok_or("no terminal")?;
    // The recorded server is still alive, but this socket cannot be contacted.
    *socket_path = rig
        .sockets
        .path()
        .join("missing.socket")
        .to_string_lossy()
        .into_owned();
    ledger.save_molecule(&id, &unavailable)?;
    let fleet_before = fs::read(rig.state().join("fleet.json"))?;
    let refused = rig
        .command(env!("CARGO_BIN_EXE_cs"))
        .args(["--json", "purge", session, "--force", "--allow-unharvested"])
        .output()?;
    assert!(
        !refused.status.success(),
        "transport errors cannot authorize fleet removal"
    );
    assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_before);
    assert!(rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", session])
        .output()?
        .status
        .success());
    // A different server at the recorded socket path must not turn an absent
    // numeric pane on that server into proof about the original live worker.
    let other_socket = rig.sockets.path().join("missing.socket");
    let other_socket = other_socket.to_str().ok_or("non UTF-8 socket")?;
    let created = rig
        .command("tmux")
        .args([
            "-S",
            other_socket,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "decoy",
            "sleep 120",
        ])
        .output()?;
    assert!(created.status.success());
    let refused = rig
        .command(env!("CARGO_BIN_EXE_cs"))
        .args(["--json", "purge", session, "--force", "--allow-unharvested"])
        .output()?;
    let other_alive = rig
        .command("tmux")
        .args(["-S", other_socket, "has-session", "-t", "decoy"])
        .output()?
        .status
        .success();
    let _ = rig
        .command("tmux")
        .args(["-S", other_socket, "kill-server"])
        .output()?;
    assert!(
        !refused.status.success(),
        "a replacement socket server cannot authorize fleet removal"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("server endpoint unavailable or changed")
    );
    assert!(other_alive);
    assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_before);
    ledger.save_molecule(&id, &original)?;
    // Extra windows make exact teardown ambiguous but do not make the live
    // original worker stale. A layout-only guard refusal must retain it.
    assert!(rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "new-window",
            "-d",
            "-t",
            session,
            "-n",
            "extra",
            "sleep 120",
        ])
        .output()?
        .status
        .success());
    let refused = rig
        .command(env!("CARGO_BIN_EXE_cs"))
        .args(["--json", "purge", session, "--force", "--allow-unharvested"])
        .output()?;
    assert!(
        !refused.status.success(),
        "layout refusal cannot establish a stale incarnation"
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("layout changed"));
    assert_eq!(fs::read(rig.state().join("fleet.json"))?, fleet_before);
    assert!(rig
        .command("tmux")
        .args([
            "-L",
            "receipt-0000",
            "kill-window",
            "-t",
            &format!("{session}:extra"),
        ])
        .output()?
        .status
        .success());
    // End the dummy executable cleanly; exact teardown must reclaim its retained pane.
    fs::write(ledger.molecule_dir(&id).join("control"), "exit 0\n")?;
    assert!(wait_for(|| !ledger
        .molecule_dir(&id)
        .join("control")
        .exists()));
    let purged: serde_json::Value = serde_json::from_slice(
        &rig.cs(&["--json", "purge", session, "--force", "--allow-unharvested"])?
            .stdout,
    )?;
    assert_eq!(purged["tmux_killed"], true);
    assert!(purged["terminal_cleanup"].is_null());
    assert!(!rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", session])
        .output()?
        .status
        .success());
    assert!(rig
        .command("tmux")
        .args(["-L", "receipt-0000", "has-session", "-t", "neighbor"])
        .output()?
        .status
        .success());
    Ok(())
}
