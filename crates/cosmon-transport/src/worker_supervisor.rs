// SPDX-License-Identifier: AGPL-3.0-only

//! One waiting parent per launch. It spawns the worker into its own process
//! group, hands that group the terminal foreground, and is the only process
//! that may stop it, through the identity-bound channel of ADR-188. It
//! installs no signal handlers and never forwards signals, restarts a child,
//! proxies a PTY or adopts a PID.

use std::io::IsTerminal;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cosmon_core::worker_exit::{
    LaunchIdentity, OwnedWorker, StopAccepted, StopRefused, SupervisionPoint, WorkerControlPort,
    WorkerExitObservation, WorkerExitReceipt, WorkerExitStore, WorkerLaunchPort,
};
use nix::errno::Errno;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

/// Grace between `SIGTERM` and `SIGKILL` of a stopped worker's group.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// Interval at which the parent polls its child; reaping happens under the
/// stop lock, so a stop can never signal a reaped (reusable) group id.
const REAP_POLL: Duration = Duration::from_millis(20);

/// Direct POSIX executable launcher, preserving inherited stdio, cwd and
/// dispositions, with the child leading a new process group.
pub struct DirectWorkerLaunch {
    /// Grace between `SIGTERM` and `SIGKILL` when the child is stopped.
    pub grace: Duration,
}

impl Default for DirectWorkerLaunch {
    fn default() -> Self {
        Self { grace: STOP_GRACE }
    }
}

struct ChildState {
    child: Child,
    exited: Option<ExitStatus>,
}

struct DirectChild {
    pid: u32,
    group: Pid,
    grace: Duration,
    state: Mutex<ChildState>,
    exit_seen: Condvar,
}

impl WorkerLaunchPort for DirectWorkerLaunch {
    fn spawn(&self, program: &str, args: &[String]) -> Result<Box<dyn OwnedWorker>, Option<i32>> {
        let child = Command::new(program)
            .args(args)
            .process_group(0)
            .spawn()
            .map_err(|error| error.raw_os_error())?;
        let pid = child.id();
        let group = Pid::from_raw(i32::try_from(pid).map_err(|_| None)?);
        hand_foreground(group);
        Ok(Box::new(DirectChild {
            pid,
            group,
            grace: self.grace,
            state: Mutex::new(ChildState {
                child,
                exited: None,
            }),
            exit_seen: Condvar::new(),
        }))
    }
}

/// Give the child's group the terminal foreground when the parent holds it,
/// then continue the group in case it touched the terminal first and was
/// stopped by `SIGTTIN`/`SIGTTOU`. Without a terminal nothing is handed over.
fn hand_foreground(group: Pid) {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return;
    }
    if nix::unistd::tcgetpgrp(&stdin) != Ok(nix::unistd::getpgrp()) {
        return;
    }
    if let Err(errno) = nix::unistd::tcsetpgrp(&stdin, group) {
        eprintln!("worker-supervise: terminal foreground not handed to worker ({errno})");
        return;
    }
    let _ = killpg(group, Signal::SIGCONT);
}

fn observe(status: ExitStatus) -> Result<WorkerExitObservation, String> {
    match (status.code(), status.signal()) {
        (Some(code), None) => u8::try_from(code)
            .map(|code| WorkerExitObservation::Exited { code })
            .map_err(|_| "invalid POSIX child exit code".to_owned()),
        (None, Some(signal)) => Ok(WorkerExitObservation::Signaled {
            signal,
            core_dumped: status.core_dumped(),
        }),
        _ => Err("owned child wait returned no terminal status".to_owned()),
    }
}

impl DirectChild {
    fn signal_group(&self, signal: Signal) -> Result<(), StopRefused> {
        match killpg(self.group, signal) {
            Ok(()) => Ok(()),
            Err(Errno::ESRCH) => Err(StopRefused::AlreadyExited),
            Err(_) => Err(StopRefused::Unsupported),
        }
    }
}

impl OwnedWorker for DirectChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn wait(&self) -> Result<WorkerExitObservation, String> {
        loop {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "owned child lock poisoned; result unknown".to_owned())?;
                if let Some(status) = state.exited {
                    return observe(status);
                }
                match state.child.try_wait() {
                    Ok(Some(status)) => {
                        state.exited = Some(status);
                        self.exit_seen.notify_all();
                        return observe(status);
                    }
                    Ok(None) => {}
                    Err(_) => return Err("owned child wait failed; result unknown".to_owned()),
                }
            }
            std::thread::sleep(REAP_POLL);
        }
    }

    fn terminate_group(
        &self,
        authorize: &dyn Fn() -> Result<(), StopRefused>,
    ) -> Result<StopAccepted, StopRefused> {
        let deadline = Instant::now() + self.grace;
        let mut state = self.state.lock().map_err(|_| StopRefused::Unsupported)?;
        authorize()?;
        if state.exited.is_some() {
            return Err(StopRefused::AlreadyExited);
        }
        self.signal_group(Signal::SIGTERM)?;
        while state.exited.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .exit_seen
                .wait_timeout(state, left)
                .map_err(|_| StopRefused::Unsupported)?
                .0;
        }
        if state.exited.is_some() {
            return Ok(StopAccepted { escalated: false });
        }
        match self.signal_group(Signal::SIGKILL) {
            Ok(()) | Err(StopRefused::AlreadyExited) => Ok(StopAccepted { escalated: true }),
            Err(refused) => Err(refused),
        }
    }
}

/// Claim, bind the stop channel, launch and wait once through injectable ports.
/// Signal death may prevent publication; the reader must then return unknown.
/// The permanent claim forbids replay.
///
/// # Errors
/// Refuses a duplicate claim, an unbindable stop channel, a lost wait result,
/// or failure to persist child evidence.
#[allow(clippy::too_many_arguments)] // One port per effect boundary.
pub fn supervise(
    identity: &LaunchIdentity,
    supervisor_pid: u32,
    program: &str,
    args: &[String],
    launcher: &dyn WorkerLaunchPort,
    control: &dyn WorkerControlPort,
    receipts: &dyn WorkerExitStore,
    checkpoint: &dyn Fn(SupervisionPoint),
) -> Result<(), String> {
    receipts.claim(supervisor_pid)?;
    // Bound before spawn: a child is never born without its stop channel. The
    // binding is dropped (and its rendezvous unlinked) after publication.
    let mut listener = control.bind(identity)?;
    checkpoint(SupervisionPoint::BeforeSpawn);
    let child: Arc<dyn OwnedWorker> = match launcher.spawn(program, args) {
        Ok(child) => Arc::from(child),
        Err(Some(errno)) => {
            return receipts.publish_observed(
                &WorkerExitReceipt {
                    identity: identity.clone(),
                    supervisor_pid,
                    child_pid: None,
                    observation: WorkerExitObservation::ExecFailed { errno },
                },
                checkpoint,
            );
        }
        Err(None) => {
            return Err("worker launch has no OS error provenance; result unknown".to_owned())
        }
    };
    checkpoint(SupervisionPoint::AfterSpawn);
    listener.serve(Arc::clone(&child));
    let child_pid = child.pid();
    // A disk error must not turn a successfully spawned child into an abandoned child.
    let birth = receipts.record_birth(supervisor_pid, child_pid);
    checkpoint(SupervisionPoint::BeforeWait);
    let observation = child.wait()?;
    checkpoint(SupervisionPoint::AfterWait);
    birth?;
    receipts.publish_observed(
        &WorkerExitReceipt {
            identity: identity.clone(),
            supervisor_pid,
            child_pid: Some(child_pid),
            observation,
        },
        checkpoint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_control::{UnixWorkerControl, UnixWorkerStop};
    use cosmon_core::worker_exit::{
        LaunchRequest, LaunchToken, WorkerControl, WorkerStopPort as _,
    };
    use cosmon_state::worker_exit::FileWorkerExitStore;
    use std::cell::Cell;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MemoryReceipts {
        claimed: Cell<bool>,
        result: Cell<WorkerExitObservation>,
        reject_birth: bool,
    }

    impl WorkerExitStore for MemoryReceipts {
        fn claim(&self, _parent: u32) -> Result<(), String> {
            if self.claimed.replace(true) {
                Err("already claimed".to_owned())
            } else {
                Ok(())
            }
        }
        fn record_birth(&self, _parent: u32, _child: u32) -> Result<(), String> {
            if self.reject_birth {
                Err("birth storage unavailable".to_owned())
            } else {
                Ok(())
            }
        }
        fn publish(&self, receipt: &WorkerExitReceipt) -> Result<(), String> {
            self.result.set(receipt.observation);
            Ok(())
        }
        fn read(&self) -> WorkerExitObservation {
            self.result.get()
        }
    }

    struct FakeLaunch {
        spawns: Cell<u32>,
        waited: Arc<AtomicBool>,
    }
    struct FakeChild(Arc<AtomicBool>);
    impl WorkerLaunchPort for FakeLaunch {
        fn spawn(
            &self,
            _program: &str,
            _args: &[String],
        ) -> Result<Box<dyn OwnedWorker>, Option<i32>> {
            self.spawns.set(self.spawns.get() + 1);
            Ok(Box::new(FakeChild(self.waited.clone())))
        }
    }
    impl OwnedWorker for FakeChild {
        fn pid(&self) -> u32 {
            456
        }
        fn wait(&self) -> Result<WorkerExitObservation, String> {
            self.0.store(true, Ordering::SeqCst);
            Ok(WorkerExitObservation::Exited { code: 7 })
        }
        fn terminate_group(
            &self,
            _authorize: &dyn Fn() -> Result<(), StopRefused>,
        ) -> Result<StopAccepted, StopRefused> {
            Err(StopRefused::Unsupported)
        }
    }

    /// Counts binds; serving is a no-op.
    struct FakeControl(Cell<u32>);
    struct FakeBinding;
    impl WorkerControl for FakeBinding {
        fn serve(&mut self, _worker: Arc<dyn OwnedWorker>) {}
    }
    impl WorkerControlPort for FakeControl {
        fn bind(
            &self,
            _identity: &LaunchIdentity,
        ) -> Result<Box<dyn cosmon_core::worker_exit::WorkerControl>, String> {
            self.0.set(self.0.get() + 1);
            Ok(Box::new(FakeBinding))
        }
    }

    fn identity_for(program: &str, args: &[String], cwd: &Path) -> Result<LaunchRequest, String> {
        LaunchRequest::bind(
            cosmon_core::id::MoleculeId::new("task-20261019-aaaa")
                .map_err(|error| error.to_string())?,
            LaunchToken::from_bytes(rand::random()),
            LaunchToken::from_bytes(rand::random()),
            program,
            args,
            cwd,
            &[],
        )
    }

    #[test]
    fn a_birth_storage_error_still_waits_and_does_not_license_a_second_spawn() -> Result<(), String>
    {
        let waited = Arc::new(AtomicBool::new(false));
        let launcher = FakeLaunch {
            spawns: Cell::new(0),
            waited: waited.clone(),
        };
        let receipts = MemoryReceipts {
            claimed: Cell::new(false),
            result: Cell::new(WorkerExitObservation::Unknown),
            reject_birth: true,
        };
        let control = FakeControl(Cell::new(0));
        let identity = identity_for("worker", &[], Path::new("/work"))?.identity;
        let run = || {
            supervise(
                &identity,
                123,
                "worker",
                &[],
                &launcher,
                &control,
                &receipts,
                &|_| {},
            )
        };
        assert!(run().is_err());
        assert!(waited.load(Ordering::SeqCst));
        assert_eq!(receipts.read(), WorkerExitObservation::Unknown);
        assert!(run().is_err());
        assert_eq!(launcher.spawns.get(), 1);
        assert_eq!(
            control.0.get(),
            1,
            "the stop channel is bound once, after the claim"
        );
        Ok(())
    }

    /// A real supervised child under a real stop channel, run on a thread.
    struct Live {
        root: tempfile::TempDir,
        identity: LaunchIdentity,
        molecule_dir: std::path::PathBuf,
        thread: std::thread::JoinHandle<Result<(), String>>,
    }

    fn launch(script: &str, grace: Duration) -> Result<Live, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let script = script.replace("$ROOT", &root.path().to_string_lossy());
        let molecule_dir = root.path().join("molecule");
        std::fs::create_dir(&molecule_dir).map_err(|error| error.to_string())?;
        let args = vec!["-c".to_owned(), script];
        let request = identity_for("/bin/sh", &args, root.path())?;
        let identity = request.identity.clone();
        let store = FileWorkerExitStore::prepare(&molecule_dir, request)?;
        let control_root = root.path().join("w");
        let thread_identity = identity.clone();
        let thread = std::thread::spawn(move || {
            supervise(
                &thread_identity,
                std::process::id(),
                "/bin/sh",
                &args,
                &DirectWorkerLaunch { grace },
                &UnixWorkerControl { root: control_root },
                &store,
                &|_| {},
            )
        });
        let live = Live {
            root,
            identity,
            molecule_dir,
            thread,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let birth = FileWorkerExitStore::open(&live.molecule_dir, &live.identity)?
            .directory()
            .join("birth.json");
        while !birth.exists() {
            if Instant::now() > deadline {
                return Err("child birth never recorded".to_owned());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(live)
    }

    /// Wait for the child's own readiness marker, written after its traps.
    fn ready(live: &Live) -> Result<(), String> {
        let marker = live.root.path().join("ready");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            if Instant::now() > deadline {
                return Err("child never became ready".to_owned());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn stop(live: &Live) -> Result<StopAccepted, StopRefused> {
        UnixWorkerStop {
            root: live.root.path().join("w"),
            reply_timeout: Duration::from_secs(10),
        }
        .request_stop(&live.identity)
    }

    fn finish(live: Live) -> Result<WorkerExitObservation, String> {
        live.thread
            .join()
            .map_err(|_| "supervisor thread panicked".to_owned())??;
        Ok(FileWorkerExitStore::open(&live.molecule_dir, &live.identity)?.read())
    }

    #[test]
    fn a_stopped_plain_child_receipts_signal_15_never_unknown() -> Result<(), String> {
        let live = launch("exec sleep 60", STOP_GRACE)?;
        assert_eq!(stop(&live), Ok(StopAccepted { escalated: false }));
        assert_eq!(
            finish(live)?,
            WorkerExitObservation::Signaled {
                signal: 15,
                core_dumped: false
            }
        );
        Ok(())
    }

    #[test]
    fn a_child_trapping_term_receipts_its_own_exit_code() -> Result<(), String> {
        let live = launch(
            "trap 'exit 42' TERM; : > '$ROOT/ready'; while :; do sleep 0.05; done",
            STOP_GRACE,
        )?;
        ready(&live)?;
        assert_eq!(stop(&live), Ok(StopAccepted { escalated: false }));
        assert_eq!(finish(live)?, WorkerExitObservation::Exited { code: 42 });
        Ok(())
    }

    #[test]
    fn a_child_ignoring_term_is_killed_after_the_grace() -> Result<(), String> {
        let live = launch(
            "trap '' TERM; : > '$ROOT/ready'; while :; do sleep 0.05; done",
            Duration::from_millis(300),
        )?;
        ready(&live)?;
        assert_eq!(stop(&live), Ok(StopAccepted { escalated: true }));
        assert_eq!(
            finish(live)?,
            WorkerExitObservation::Signaled {
                signal: 9,
                core_dumped: false
            }
        );
        Ok(())
    }

    #[test]
    fn a_grandchild_in_the_group_dies_with_the_worker() -> Result<(), String> {
        let live = launch(
            "sleep 60 & echo $! > '$ROOT/grandchild.pid'; wait",
            STOP_GRACE,
        )?;
        let pid_file = live.root.path().join("grandchild.pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break Pid::from_raw(pid);
            }
            if Instant::now() > deadline {
                return Err("grandchild pid never written".to_owned());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(nix::sys::signal::kill(grandchild, None).is_ok());
        assert_eq!(stop(&live), Ok(StopAccepted { escalated: false }));
        assert!(finish(live)?.is_terminal());
        let deadline = Instant::now() + Duration::from_secs(5);
        while nix::sys::signal::kill(grandchild, None).is_ok() {
            assert!(
                Instant::now() < deadline,
                "grandchild survived its group's stop"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    #[test]
    fn a_stop_after_the_child_exited_is_refused_and_the_receipt_stands() -> Result<(), String> {
        let live = launch("exit 3", STOP_GRACE)?;
        let identity = live.identity.clone();
        let control_root = live.root.path().join("w");
        let root = live.root;
        let molecule_dir = live.molecule_dir;
        live.thread
            .join()
            .map_err(|_| "supervisor thread panicked".to_owned())??;
        let outcome = crate::worker_control::stop_launch(
            &molecule_dir,
            &identity,
            &UnixWorkerStop {
                root: control_root,
                reply_timeout: Duration::from_secs(5),
            },
            Duration::from_secs(1),
        )?;
        assert_eq!(outcome.evidence_warning, None);
        assert_eq!(
            outcome.outcome,
            cosmon_core::worker_exit::SupervisedStop::Stopped(WorkerExitObservation::Exited {
                code: 3
            })
        );
        drop(root);
        Ok(())
    }
}
