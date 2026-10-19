// SPDX-License-Identifier: AGPL-3.0-only

//! One waiting parent per launch. No signal handlers, forwarding, new process group,
//! PTY proxy, automatic restart, or PID-based adoption is introduced here.

use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command};

use cosmon_core::worker_exit::{
    LaunchIdentity, OwnedWorker, SupervisionPoint, WorkerExitObservation, WorkerExitReceipt,
    WorkerExitStore, WorkerLaunchPort,
};

/// Direct POSIX executable launcher, preserving inherited stdio, cwd and dispositions.
pub struct DirectWorkerLaunch;

struct DirectChild(Child);

impl WorkerLaunchPort for DirectWorkerLaunch {
    fn spawn(&self, program: &str, args: &[String]) -> Result<Box<dyn OwnedWorker>, Option<i32>> {
        Command::new(program)
            .args(args)
            .spawn()
            .map(|child| Box::new(DirectChild(child)) as Box<dyn OwnedWorker>)
            .map_err(|error| error.raw_os_error())
    }
}

impl OwnedWorker for DirectChild {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn wait(&mut self) -> Result<WorkerExitObservation, String> {
        let status = self
            .0
            .wait()
            .map_err(|_| "owned child wait failed; result unknown".to_owned())?;
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
}

/// Claim, launch and wait once through injectable ports. Signal death may prevent
/// publication; the reader must then return unknown. The permanent claim forbids replay.
///
/// # Errors
/// Refuses a duplicate claim, lost wait result, or failure to persist child evidence.
pub fn supervise(
    identity: &LaunchIdentity,
    supervisor_pid: u32,
    program: &str,
    args: &[String],
    launcher: &dyn WorkerLaunchPort,
    receipts: &dyn WorkerExitStore,
    checkpoint: &dyn Fn(SupervisionPoint),
) -> Result<(), String> {
    receipts.claim(supervisor_pid)?;
    checkpoint(SupervisionPoint::BeforeSpawn);
    let mut child = match launcher.spawn(program, args) {
        Ok(child) => child,
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
    use std::{cell::Cell, rc::Rc};

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
        waited: Rc<Cell<bool>>,
    }
    struct FakeChild(Rc<Cell<bool>>);
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
        fn wait(&mut self) -> Result<WorkerExitObservation, String> {
            self.0.set(true);
            Ok(WorkerExitObservation::Exited { code: 7 })
        }
    }

    #[test]
    fn a_birth_storage_error_still_waits_and_does_not_license_a_second_spawn() -> Result<(), String>
    {
        let waited = Rc::new(Cell::new(false));
        let launcher = FakeLaunch {
            spawns: Cell::new(0),
            waited: waited.clone(),
        };
        let receipts = MemoryReceipts {
            claimed: Cell::new(false),
            result: Cell::new(WorkerExitObservation::Unknown),
            reject_birth: true,
        };
        let identity = cosmon_core::worker_exit::LaunchRequest::bind(
            cosmon_core::id::MoleculeId::new("task-20261019-aaaa")
                .map_err(|error| error.to_string())?,
            cosmon_core::worker_exit::LaunchToken::from_bytes([1; 16]),
            cosmon_core::worker_exit::LaunchToken::from_bytes([2; 16]),
            "worker",
            &[],
            std::path::Path::new("/work"),
            &[],
        )?
        .identity;
        assert!(supervise(&identity, 123, "worker", &[], &launcher, &receipts, &|_| {}).is_err());
        assert!(waited.get());
        assert_eq!(receipts.read(), WorkerExitObservation::Unknown);
        assert!(supervise(&identity, 123, "worker", &[], &launcher, &receipts, &|_| {}).is_err());
        assert_eq!(launcher.spawns.get(), 1);
        Ok(())
    }
}
