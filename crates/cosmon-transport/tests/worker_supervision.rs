// SPDX-License-Identifier: AGPL-3.0-only

//! Real process crashes at the six durable supervision boundaries. These tests
//! use owned subprocess handles and disposable state, never a PID read from disk.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_core::id::MoleculeId;
use cosmon_core::worker_exit::{
    LaunchRequest, LaunchToken, SupervisionPoint, WorkerExitObservation, WorkerExitStore,
};
use cosmon_state::worker_exit::FileWorkerExitStore;
use cosmon_transport::worker_supervisor::{supervise, DirectWorkerLaunch};

type TestResult = Result<(), Box<dyn Error>>;

fn command_args(root: &Path) -> Vec<String> {
    vec![
        "-c".to_owned(),
        "printf 'start\n' >> \"$1\"; exit 7".to_owned(),
        "witness".to_owned(),
        root.join("starts").to_string_lossy().into_owned(),
    ]
}

fn prepare(root: &Path) -> Result<FileWorkerExitStore, Box<dyn Error>> {
    Ok(FileWorkerExitStore::prepare(
        root,
        LaunchRequest::bind(
            MoleculeId::new("task-20261019-aaaa")?,
            LaunchToken::from_bytes([1; 16]),
            LaunchToken::from_bytes([2; 16]),
            "/bin/sh",
            &command_args(root),
            root,
            &[],
        )?,
    )?)
}

struct Helper(Child);

impl Helper {
    fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }
    fn wait_bounded(&mut self) -> Result<std::process::ExitStatus, Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("supervision test helper stalled".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_helper(root: &Path, point: &str) -> Result<Helper, Box<dyn Error>> {
    Ok(Helper(
        Command::new(std::env::current_exe()?)
            .args(["--ignored", "--exact", "crash_child", "--nocapture"])
            .env("WORKER_CRASH_ROOT", root)
            .env("WORKER_CRASH_POINT", point)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ))
}

fn wait_for(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
#[ignore = "subprocess entry point; the crash matrix invokes this test with private inputs"]
fn crash_child() -> TestResult {
    let root = std::path::PathBuf::from(std::env::var("WORKER_CRASH_ROOT")?);
    let point = std::env::var("WORKER_CRASH_POINT")?;
    let store = FileWorkerExitStore::current(&root)?.ok_or("missing prepared launch")?;
    supervise(
        &store.request().identity,
        std::process::id(),
        "/bin/sh",
        &command_args(&root),
        &DirectWorkerLaunch,
        &store,
        &|observed| {
            if format!("{observed:?}") == point {
                let barrier = root.join("barrier");
                if fs::write(
                    root.join(format!("barrier-{}", std::process::id())),
                    b"reached",
                )
                .is_ok()
                    && fs::write(&barrier, std::process::id().to_string()).is_ok()
                {
                    loop {
                        std::thread::park();
                    }
                }
            }
        },
    )?;
    Ok(())
}

#[test]
fn six_crash_windows_reconstruct_from_disk_without_replaying_spawn() -> TestResult {
    for point in [
        SupervisionPoint::BeforeSpawn,
        SupervisionPoint::AfterSpawn,
        SupervisionPoint::BeforeWait,
        SupervisionPoint::AfterWait,
        SupervisionPoint::AfterTemporarySync,
        SupervisionPoint::AfterDurableResult,
    ] {
        let root = tempfile::tempdir()?;
        let store = prepare(root.path())?;
        let mut helper = start_helper(root.path(), &format!("{point:?}"))?;
        let reached = wait_for(|| root.path().join("barrier").exists());
        // Always reap our own helper, including on a failed barrier assertion.
        helper.kill()?;
        helper.wait_bounded()?;
        assert!(reached, "checkpoint {point:?} was not reached");
        let expected = if point == SupervisionPoint::AfterDurableResult {
            WorkerExitObservation::Exited { code: 7 }
        } else {
            WorkerExitObservation::Unknown
        };
        assert_eq!(store.read(), expected, "checkpoint {point:?}");
        assert_eq!(
            FileWorkerExitStore::current(root.path())?
                .ok_or("missing reconstructed store")?
                .read(),
            expected
        );
        assert!(
            store.claim(std::process::id()).is_err(),
            "a crash must not release its claim"
        );
        if point != SupervisionPoint::BeforeSpawn {
            assert!(wait_for(|| fs::read_to_string(root.path().join("starts"))
                .is_ok_and(|text| text == "start\n")));
        } else {
            assert!(!root.path().join("starts").exists());
        }
        let status = start_helper(root.path(), "BeforeSpawn")?.wait_bounded()?;
        assert!(!status.success(), "retry must refuse before spawning");
        assert_eq!(
            fs::read_to_string(root.path().join("starts"))
                .unwrap_or_default()
                .lines()
                .count(),
            usize::from(point != SupervisionPoint::BeforeSpawn)
        );
    }
    Ok(())
}

#[test]
fn four_initial_contenders_spawn_at_most_one_child_and_all_retries_refuse() -> TestResult {
    let root = tempfile::tempdir()?;
    let store = prepare(root.path())?;
    let mut contenders: Vec<_> = (0..4)
        .map(|_| start_helper(root.path(), "AfterDurableResult"))
        .collect::<Result<_, _>>()?;
    let reached = wait_for(|| {
        contenders.iter_mut().all(|child| {
            child.0.try_wait().is_ok_and(|status| status.is_some())
                || root
                    .path()
                    .join(format!("barrier-{}", child.0.id()))
                    .exists()
        })
    });
    // Kill every owned helper, including multiple winners under a broken claim implementation.
    for child in &mut contenders {
        let _ = child.kill();
    }
    for child in &mut contenders {
        assert!(!child.wait_bounded()?.success());
    }
    assert!(reached);
    assert_eq!(store.read(), WorkerExitObservation::Exited { code: 7 });
    for _ in 0..4 {
        assert!(!start_helper(root.path(), "BeforeSpawn")?
            .wait_bounded()?
            .success());
    }
    assert_eq!(fs::read_to_string(root.path().join("starts"))?, "start\n");
    Ok(())
}

#[test]
fn direct_wait_keeps_exit_143_signal_15_and_exec_failure_distinct() -> TestResult {
    for (program, args, expected) in [
        (
            "/bin/sh",
            vec!["-c".to_owned(), "exit 143".to_owned()],
            WorkerExitObservation::Exited { code: 143 },
        ),
        (
            "/bin/sh",
            vec!["-c".to_owned(), "kill -TERM $$".to_owned()],
            WorkerExitObservation::Signaled {
                signal: 15,
                core_dumped: false,
            },
        ),
        (
            "/definitely-absent-worker",
            vec![],
            WorkerExitObservation::ExecFailed { errno: 2 },
        ),
    ] {
        let root = tempfile::tempdir()?;
        let store = prepare(root.path())?;
        supervise(
            &store.request().identity,
            std::process::id(),
            program,
            &args,
            &DirectWorkerLaunch,
            &store,
            &|_| {},
        )?;
        assert_eq!(store.read(), expected);
    }
    Ok(())
}
