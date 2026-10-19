// SPDX-License-Identifier: AGPL-3.0-only

//! A different injected implementation must receive the actual dispatch effects.

use super::*;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportError,
};
use std::cell::RefCell;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Default)]
struct CommandBackend {
    inner: cosmon_transport::MockBackend,
    commands: RefCell<Vec<String>>,
    hooks: RefCell<Vec<String>>,
    failure_with_neighbor: std::cell::Cell<bool>,
    missing_identity: std::cell::Cell<bool>,
    readiness_failure: std::cell::Cell<bool>,
    replaced: std::cell::Cell<bool>,
}

fn identity() -> cosmon_core::terminal::TerminalIdentity {
    cosmon_core::terminal::TerminalIdentity::Tmux {
        socket_path: "/isolated/fake-transport.sock".into(),
        server_pid: 1,
        server_start: 2,
        session_id: 3,
        session_created: 4,
        pane_id: 5,
        pane_pid: 6,
        pane_start: 7,
    }
}

impl TransportBackend for CommandBackend {
    fn supports_command_sessions(&self) -> bool {
        true
    }
    fn spawn_command(
        &self,
        session: &str,
        directory: &str,
        command: &str,
        _: &[(String, String)],
    ) -> Result<Option<cosmon_core::terminal::TerminalIdentity>, TransportError> {
        self.commands.borrow_mut().push(command.into());
        self.inner.spawn(
            &AgentDefinition {
                id: cosmon_core::id::AgentId::new(session)
                    .map_err(|e| TransportError::Io(e.to_string()))?,
                role: AgentRole::Implementation,
                command: command.into(),
                args: vec![],
                cwd: Some(directory.into()),
            },
            &RuntimeConfig::default(),
        )?;
        if self.failure_with_neighbor.get() {
            return Err(TransportError::SpawnFailed(
                "foreign session won the creation race".into(),
            ));
        }
        Ok((!self.missing_identity.get()).then(identity))
    }
    fn install_exit_hook(&self, _: &str, command: &str) -> Result<(), TransportError> {
        self.hooks.borrow_mut().push(command.into());
        Ok(())
    }
    fn spawn(&self, a: &AgentDefinition, c: &RuntimeConfig) -> Result<SpawnHandle, TransportError> {
        self.inner.spawn(a, c)
    }
    fn terminate(&self, w: &cosmon_core::id::WorkerId) -> Result<(), TransportError> {
        self.inner.terminate(w)
    }
    fn is_alive(&self, w: &cosmon_core::id::WorkerId) -> Result<bool, TransportError> {
        self.inner.is_alive(w)
    }
    fn send_input(&self, w: &cosmon_core::id::WorkerId, s: &str) -> Result<(), TransportError> {
        self.inner.send_input(w, s)
    }
    fn capture_output(
        &self,
        w: &cosmon_core::id::WorkerId,
        n: usize,
    ) -> Result<String, TransportError> {
        if self.readiness_failure.get() {
            if !self.replaced.replace(true) {
                let state = std::env::current_dir()
                    .map_err(|e| TransportError::Io(e.to_string()))?
                    .join(".cosmon/state");
                let store = FileStore::new(state);
                let id =
                    MoleculeId::new(w.as_str()).map_err(|e| TransportError::Io(e.to_string()))?;
                let mut molecule = store
                    .load_molecule(&id)
                    .map_err(|e| TransportError::Io(e.to_string()))?;
                if let Some(process) = molecule.process.as_mut() {
                    process.started_at += chrono::Duration::seconds(1);
                    let mut replacement = identity();
                    let cosmon_core::terminal::TerminalIdentity::Tmux { pane_id, .. } =
                        &mut replacement;
                    *pane_id = 99;
                    process.terminal = Some(replacement);
                }
                store
                    .save_molecule(&id, &molecule)
                    .map_err(|e| TransportError::Io(e.to_string()))?;
            }
            return Err(TransportError::Io(
                "replacement won during readiness".into(),
            ));
        }
        self.inner.capture_output(w, n)
    }
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        self.inner.list_sessions()
    }
    fn graceful_exit(
        &self,
        w: &cosmon_core::id::WorkerId,
        d: std::time::Duration,
    ) -> Result<bool, TransportError> {
        self.inner.graceful_exit(w, d)
    }
}

#[test]
fn configured_dispatch_reaches_the_injected_command_backend() -> TestResult {
    const CHILD: &str = "COSMON_TRANSPORT_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        use clap::Parser as _;
        let root = std::env::current_dir()?;
        let state = root.join(".cosmon/state");
        let store = FileStore::new(&state);
        let mut mol = sample_molecule("witness-20261019-abcd", MoleculeStatus::Pending);
        mol.formula_id = FormulaId::new("witness")?;
        store.save_molecule(&mol.id, &mol)?;
        let cli = crate::Cli::try_parse_from([
            "cs",
            "tackle",
            mol.id.as_str(),
            "--adapter",
            "codex",
            "--name",
            "injected-worker",
        ])?;
        let crate::Command::Tackle(args) = cli.command else {
            return Err("wrong test command".into());
        };
        let backend = CommandBackend::default();
        backend.inner.set_canned_output("OpenAI Codex\nready\n");
        let ctx = Context {
            config: Some(state.clone()),
            json: true,
            verbose: false,
        };
        let before = std::fs::read(store.molecule_dir(&mol.id).join("state.json"))?;
        assert!(
            run_with_transport(&ctx, &args, Some(&cosmon_transport::MockBackend::new())).is_err()
        );
        assert_eq!(
            std::fs::read(store.molecule_dir(&mol.id).join("state.json"))?,
            before
        );
        assert!(!root.join(".worktrees").exists());
        run_with_transport(&ctx, &args, Some(&backend))?;
        assert_eq!(backend.commands.borrow().len(), 1);
        assert!(backend.commands.borrow()[0].contains("exec"));
        assert_eq!(backend.hooks.borrow().len(), 1);
        assert!(backend.hooks.borrow()[0].contains("--terminal-identity"));
        let actual = store
            .load_molecule(&mol.id)?
            .process
            .ok_or("dispatch not persisted")?;
        assert_eq!(
            actual.transport.map(|t| t.source),
            Some(cosmon_core::transport_selection::TransportSource::Project)
        );
        assert_eq!(actual.terminal, Some(identity()));
        assert!(
            cosmon_state::event_log::read_all(state.join("events.jsonl"))?
                .iter()
                .any(|e| matches!(
                    e.event,
                    cosmon_core::event_v2::EventV2::WorkerSpawned {
                        transport: Some(_),
                        ..
                    }
                ))
        );

        for (id, missing) in [
            ("witness-20261019-abce", false),
            ("witness-20261019-abcf", true),
        ] {
            let mut other = sample_molecule(id, MoleculeStatus::Pending);
            other.formula_id = FormulaId::new("witness")?;
            store.save_molecule(&other.id, &other)?;
            let cli = crate::Cli::try_parse_from([
                "cs",
                "tackle",
                id,
                "--adapter",
                "codex",
                "--name",
                id,
            ])?;
            let crate::Command::Tackle(args) = cli.command else {
                return Err("wrong command".into());
            };
            let failing = CommandBackend::default();
            failing.inner.set_canned_output("OpenAI Codex\nready\n");
            failing.failure_with_neighbor.set(!missing);
            failing.missing_identity.set(missing);
            assert!(run_with_transport(&ctx, &args, Some(&failing)).is_err());
            if missing {
                let unknown = store
                    .load_molecule(&other.id)?
                    .process
                    .ok_or("unresolved dispatch lost")?;
                assert!(unknown.transport.is_some() && unknown.terminal.is_none());
            } else {
                let unknown = store
                    .load_molecule(&other.id)?
                    .process
                    .ok_or("attempted launch intention erased")?;
                assert!(unknown.transport.is_some() && unknown.terminal.is_none());
                assert!(root.join(".worktrees").join(id).exists());
                // The late same-name occupant is foreign to our failed launch.
                assert!(failing
                    .inner
                    .is_alive(&cosmon_core::id::WorkerId::new(id)?)?);
                assert!(!failing.inner.calls().iter().any(|call| matches!(
                    call,
                    cosmon_transport::mock::MockCall::Terminate { .. }
                )));
            }
        }

        let id = "witness-20261019-abd0";
        let mut replacement_case = sample_molecule(id, MoleculeStatus::Pending);
        replacement_case.formula_id = FormulaId::new("witness")?;
        store.save_molecule(&replacement_case.id, &replacement_case)?;
        let cli =
            crate::Cli::try_parse_from(["cs", "tackle", id, "--adapter", "codex", "--name", id])?;
        let crate::Command::Tackle(args) = cli.command else {
            return Err("wrong command".into());
        };
        let raced = CommandBackend::default();
        raced.readiness_failure.set(true);
        let error = run_with_transport(&ctx, &args, Some(&raced))
            .err()
            .ok_or("readiness failure was accepted")?;
        assert!(error.downcast_ref::<ExecLaunchUnresolved>().is_some());
        let observed = store
            .load_molecule(&replacement_case.id)?
            .process
            .ok_or("replacement was erased")?;
        assert!(matches!(
            observed.terminal,
            Some(cosmon_core::terminal::TerminalIdentity::Tmux { pane_id: 99, .. })
        ));
        assert!(root.join(".worktrees").join(id).exists());
        assert!(!raced
            .inner
            .calls()
            .iter()
            .any(|call| matches!(call, cosmon_transport::mock::MockCall::Terminate { .. })));
        assert!(!cosmon_state::event_log::read_all(state.join("events.jsonl"))?.iter().any(|event| matches!(&event.event, cosmon_core::event_v2::EventV2::WorkerSpawnRolledBack { mol_id, .. } if mol_id.as_str() == id)));
        return Ok(());
    }
    // Process isolation avoids changing CWD, PATH or HOME for other Rust tests.
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join(".cosmon/formulas"))?;
    std::fs::create_dir(root.path().join("bin"))?;
    std::fs::write(root.path().join(".cosmon/config.toml"), "[project]\nproject_id='injected-0000'\n[transport]\nbackend='tmux'\n[adapters.codex]\nmode='exec'\nupdate='skip'\n")?;
    std::fs::write(root.path().join(".cosmon/formulas/witness.formula.toml"), "formula='witness'\nversion=1\ndescription='Transport injection example'\n[[steps]]\nid='work'\ntitle='Work'\ndescription='No model'\nacceptance='Recorded dispatch'\n")?;
    let codex = root.path().join("bin/codex");
    std::fs::write(&codex, "#!/bin/sh\nprintf 'codex 0.0.0\\n'\n")?;
    std::fs::set_permissions(codex, std::fs::Permissions::from_mode(0o700))?;
    for args in [
        vec!["init", "-q"],
        vec!["add", ".cosmon"],
        vec![
            "-c",
            "user.name=Noogram",
            "-c",
            "user.email=test@example.org",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "test: initialize transport injection",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(root.path())
            .args(args)
            .status()?
            .success());
    }
    let result = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "cmd::tackle::tests::worker_transport::configured_dispatch_reaches_the_injected_command_backend", "--nocapture"])
        .current_dir(root.path()).env_clear().env(CHILD, "1").env("HOME", root.path())
        .env("COSMON_CONFIG_HOME", root.path().join("global"))
        .env("PATH", format!("{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin", root.path().join("bin").display()))
        .output()?;
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
