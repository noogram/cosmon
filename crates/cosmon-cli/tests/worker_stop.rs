// SPDX-License-Identifier: AGPL-3.0-only

//! ADR-188 end to end: the real `cs worker-supervise` parent, its real stop
//! channel, and the client that `cs purge --force` and `cs done` use.
//!
//! Each test prepares a launch exactly as `cs tackle` does (a
//! `FileWorkerExitStore` in a molecule directory), runs the supervisor with
//! `HOME` pointed at a private directory so its rendezvous socket is private
//! too, and stops the worker by identity only.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_core::id::MoleculeId;
use cosmon_core::worker_exit::{
    LaunchIdentity, LaunchRequest, LaunchToken, StopRefused, SupervisedStop, WorkerExitObservation,
    WorkerExitStore as _,
};
use cosmon_state::worker_exit::FileWorkerExitStore;
use cosmon_transport::worker_control::{stop_launch, UnixWorkerStop};
use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One prepared launch in a private directory tree.
struct Launch {
    _root: tempfile::TempDir,
    /// `HOME` lives under `/tmp`: a socket under macOS's per-user temporary
    /// directory would exceed the 104-byte `sun_path` limit.
    _home: tempfile::TempDir,
    /// Canonical working directory; also where the worker writes markers.
    cwd: PathBuf,
    home: PathBuf,
    molecule_dir: PathBuf,
    identity: LaunchIdentity,
    args: Vec<String>,
}

impl Launch {
    /// Prepare a launch of `/bin/sh -c <script>`; `$ROOT` expands to `cwd`.
    fn prepare(script: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let cwd = root.path().canonicalize()?;
        let home_dir = tempfile::tempdir_in("/tmp")?;
        let home = home_dir.path().canonicalize()?;
        let molecule_dir = cwd.join("molecule");
        std::fs::create_dir_all(&molecule_dir)?;
        let args = vec![
            "-c".to_owned(),
            script.replace("$ROOT", &cwd.to_string_lossy()),
        ];
        let request = LaunchRequest::bind(
            MoleculeId::new("task-20261020-aaaa")?,
            LaunchToken::from_bytes(rand::random()),
            LaunchToken::from_bytes(rand::random()),
            "/bin/sh",
            &args,
            &cwd,
            &[],
        )?;
        let identity = request.identity.clone();
        FileWorkerExitStore::prepare(&molecule_dir, request)?;
        Ok(Self {
            _root: root,
            _home: home_dir,
            cwd,
            home,
            molecule_dir,
            identity,
            args,
        })
    }

    /// The supervisor's argv, as `spawn_codex` builds it.
    fn supervisor_argv(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut argv = vec![
            env!("CARGO_BIN_EXE_cs").to_owned(),
            "worker-supervise".to_owned(),
            "--molecule-dir".to_owned(),
            self.molecule_dir.to_string_lossy().into_owned(),
            "--identity".to_owned(),
            serde_json::to_string(&self.identity)?,
            "--program".to_owned(),
            "/bin/sh".to_owned(),
            "--".to_owned(),
        ];
        argv.extend(self.args.iter().cloned());
        Ok(argv)
    }

    fn spawn_supervisor(&self) -> Result<Child, Box<dyn std::error::Error>> {
        let argv = self.supervisor_argv()?;
        Ok(Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .spawn()?)
    }

    fn stop(&self) -> Result<cosmon_transport::worker_control::StopReport, String> {
        stop_launch(
            &self.molecule_dir,
            &self.identity,
            &UnixWorkerStop {
                root: self.home.join(".cosmon").join("run").join("w"),
                reply_timeout: Duration::from_secs(15),
            },
            Duration::from_secs(15),
        )
    }

    fn receipt(&self) -> Result<WorkerExitObservation, String> {
        Ok(FileWorkerExitStore::open(&self.molecule_dir, &self.identity)?.read())
    }

    /// PID the worker wrote to `$ROOT/pid` once ready.
    fn worker_pid(&self) -> Result<Pid, Box<dyn std::error::Error>> {
        let path = self.cwd.join("pid");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(pid) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                return Ok(Pid::from_raw(pid));
            }
            if Instant::now() > deadline {
                return Err("worker never wrote its pid".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("supervisor did not exit in time".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn alive(pid: Pid) -> bool {
    kill(pid, None).is_ok()
}

const WORKER: &str = "echo $$ > '$ROOT/pid.tmp'; mv '$ROOT/pid.tmp' '$ROOT/pid'; exec sleep 60";

#[test]
fn a_sigkilled_supervisor_is_unreachable_and_nothing_falls_back() -> TestResult {
    let launch = Launch::prepare(WORKER)?;
    let mut supervisor = launch.spawn_supervisor()?;
    let worker = launch.worker_pid()?;
    supervisor.kill()?;
    supervisor.wait()?;

    let report = launch.stop()?;
    assert_eq!(
        report.outcome,
        SupervisedStop::Refused(StopRefused::Unreachable)
    );
    assert!(!cosmon_transport::worker_control::permits_teardown(&Ok(
        report.clone()
    )));
    assert_eq!(launch.receipt()?, WorkerExitObservation::Unknown);
    // No fallback: the orphaned worker is still running after the refusal.
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(worker), "a refused stop must not kill the worker");
    let evidence = std::fs::read_to_string(
        FileWorkerExitStore::open(&launch.molecule_dir, &launch.identity)?
            .directory()
            .join("stop-requests.jsonl"),
    )?;
    assert!(evidence.contains("\"unreachable\""), "{evidence}");

    killpg(worker, Signal::SIGKILL)?;
    Ok(())
}

#[test]
fn a_stop_through_the_real_supervisor_receipts_sigterm() -> TestResult {
    let launch = Launch::prepare(WORKER)?;
    let mut supervisor = launch.spawn_supervisor()?;
    let worker = launch.worker_pid()?;
    let report = launch.stop()?;
    assert_eq!(
        report.outcome,
        SupervisedStop::Stopped(WorkerExitObservation::Signaled {
            signal: 15,
            core_dumped: false
        })
    );
    wait_exit(&mut supervisor, Duration::from_secs(10))?;
    assert!(!alive(worker));
    // Idempotent: a second stop finds the terminal receipt and sends nothing.
    let again = launch.stop()?;
    assert_eq!(again.outcome, report.outcome);
    let path = FileWorkerExitStore::open(&launch.molecule_dir, &launch.identity)?
        .directory()
        .join("stop-requests.jsonl");
    assert_eq!(std::fs::read_to_string(path)?.lines().count(), 1);
    Ok(())
}

fn tmux(socket: &str, args: &[&str]) -> Result<std::process::Output, std::io::Error> {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Start `launch`'s supervisor in session `worker` on its own tmux server.
fn tmux_launch(socket: &str, launch: &Launch) -> TestResult {
    let mut command = vec![
        "exec".to_owned(),
        "env".to_owned(),
        format!("HOME={}", shell_quote(&launch.home.to_string_lossy())),
    ];
    command.extend(launch.supervisor_argv()?.iter().map(|arg| shell_quote(arg)));
    let output = tmux(
        socket,
        &[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "worker",
            "-c",
            &launch.cwd.to_string_lossy(),
            &command.join(" "),
        ],
    )?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(())
}

#[test]
fn stopping_launch_a_leaves_same_name_launch_b_intact() -> TestResult {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not installed; skipping");
        return Ok(());
    }
    let suffix: u32 = rand::random();
    let (socket_a, socket_b) = (format!("cs-stop-a-{suffix}"), format!("cs-stop-b-{suffix}"));
    let (a, b) = (Launch::prepare(WORKER)?, Launch::prepare(WORKER)?);
    let result = (|| -> TestResult {
        tmux_launch(&socket_a, &a)?;
        tmux_launch(&socket_b, &b)?;
        let (worker_a, worker_b) = (a.worker_pid()?, b.worker_pid()?);

        let report = a.stop()?;
        assert_eq!(
            report.outcome,
            SupervisedStop::Stopped(WorkerExitObservation::Signaled {
                signal: 15,
                core_dumped: false
            })
        );
        assert!(!alive(worker_a));
        assert!(alive(worker_b), "stopping A must leave same-name B running");
        assert_eq!(b.receipt()?, WorkerExitObservation::Unknown);
        assert!(tmux(&socket_b, &["has-session", "-t", "worker"])?
            .status
            .success());

        let report = b.stop()?;
        assert!(matches!(report.outcome, SupervisedStop::Stopped(_)));
        Ok(())
    })();
    let _ = tmux(&socket_a, &["kill-server"]);
    let _ = tmux(&socket_b, &["kill-server"]);
    result
}

#[test]
fn a_worker_reading_its_terminal_gets_the_foreground() -> TestResult {
    // Under a pseudo-terminal the supervisor is the foreground group. A
    // worker in its own group that reads the terminal would be stopped by
    // SIGTTIN, and the supervisor would wait forever, unless it was handed
    // the foreground.
    let launch = Launch::prepare("read x; [ \"$x\" = ping ] && exit 42; exit 1")?;
    let argv = launch.supervisor_argv()?;
    let mut script = Command::new("script");
    if cfg!(target_os = "macos") {
        script.arg("-q").arg("/dev/null").args(&argv);
    } else {
        let line: Vec<String> = argv.iter().map(|arg| shell_quote(arg)).collect();
        script.arg("-qec").arg(line.join(" ")).arg("/dev/null");
    }
    let mut child = script
        .current_dir(&launch.cwd)
        .env("HOME", &launch.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    // Wait for the worker's birth so the line reaches its read.
    let birth: &Path = &FileWorkerExitStore::open(&launch.molecule_dir, &launch.identity)?
        .directory()
        .join("birth.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !birth.exists() {
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("worker never born".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::io::Write::write_all(&mut stdin, b"ping\n")?;
    wait_exit(&mut child, Duration::from_secs(20))?;
    drop(stdin);
    assert_eq!(
        launch.receipt()?,
        WorkerExitObservation::Exited { code: 42 }
    );
    Ok(())
}
