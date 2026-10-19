// SPDX-License-Identifier: AGPL-3.0-only

//! Identity-bound stop channel between a teardown caller and the supervising
//! parent of one launch (ADR-188).
//!
//! The Unix socket path is a rendezvous derived from the identity; it never
//! authorises anything. Authority is the equality of the requested
//! [`LaunchIdentity`](cosmon_core::worker_exit::LaunchIdentity) with the
//! supervisor's own, checked under the child's stop lock, between two
//! processes of the same effective UID. One connection carries one request
//! line and one reply line.

/// Outcome of a supervised stop, with any failure to write its audit evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// What the teardown caller may do next.
    pub outcome: cosmon_core::worker_exit::SupervisedStop,
    /// Set when `stop-requests.jsonl` could not be appended; never changes `outcome`.
    pub evidence_warning: Option<String>,
}

impl StopReport {
    /// Whether the caller may tear down the launch's (dead) terminal: only
    /// when the launch has a terminal receipt. Every other outcome performs
    /// no fallback termination.
    #[must_use]
    pub fn permits_teardown(&self) -> bool {
        matches!(
            self.outcome,
            cosmon_core::worker_exit::SupervisedStop::Stopped(_)
        )
    }
}

/// Whether a supervised stop attempt (or its failure to start) permits teardown.
#[must_use]
pub fn permits_teardown(stop: &Result<StopReport, String>) -> bool {
    stop.as_ref().is_ok_and(StopReport::permits_teardown)
}

/// One operator-facing line for a supervised stop attempt. A refusal never
/// claims the worker is dead: it says no fallback termination was performed.
#[must_use]
pub fn describe(stop: &Result<StopReport, String>) -> String {
    use cosmon_core::worker_exit::SupervisedStop;
    const UNRESOLVED: &str = "no fallback termination performed; launch exit left unresolved";
    let line = match stop {
        Ok(report) => match report.outcome {
            SupervisedStop::Stopped(observation) => {
                format!("supervised worker stopped; receipt {observation:?}")
            }
            SupervisedStop::Refused(refused) => {
                format!("warning: supervised stop refused ({refused}); {UNRESOLVED}")
            }
            SupervisedStop::Pending => {
                format!("warning: supervised stop accepted but no receipt yet; {UNRESOLVED}")
            }
        },
        Err(error) => {
            format!("warning: supervised launch evidence unavailable ({error}); {UNRESOLVED}")
        }
    };
    match stop
        .as_ref()
        .ok()
        .and_then(|report| report.evidence_warning.as_deref())
    {
        Some(warning) => format!("{line}; warning: {warning}"),
        None => line,
    }
}

/// Stop the launch through the default rendezvous root (see [`stop_launch`]).
///
/// # Errors
/// Fails when `HOME` is unset or the launch evidence cannot be opened.
#[cfg(unix)]
pub fn stop_launch_from_env(
    molecule_dir: &std::path::Path,
    identity: &cosmon_core::worker_exit::LaunchIdentity,
) -> Result<StopReport, String> {
    stop_launch(
        molecule_dir,
        identity,
        &UnixWorkerStop::from_env()?,
        SETTLE_TIMEOUT,
    )
}

/// Without Unix sockets no supervised stop exists; refuse, killing nothing.
///
/// # Errors
/// Never; the refusal is the outcome.
#[cfg(not(unix))]
pub fn stop_launch_from_env(
    _molecule_dir: &std::path::Path,
    _identity: &cosmon_core::worker_exit::LaunchIdentity,
) -> Result<StopReport, String> {
    Ok(StopReport {
        outcome: cosmon_core::worker_exit::SupervisedStop::Refused(
            cosmon_core::worker_exit::StopRefused::Unsupported,
        ),
        evidence_warning: None,
    })
}

#[cfg(test)]
mod decision_tests {
    use super::{describe, permits_teardown, StopReport};
    use cosmon_core::worker_exit::{StopRefused, SupervisedStop, WorkerExitObservation};

    fn report(outcome: SupervisedStop) -> Result<StopReport, String> {
        Ok(StopReport {
            outcome,
            evidence_warning: None,
        })
    }

    #[test]
    fn only_a_terminal_receipt_permits_teardown() {
        let stopped = report(SupervisedStop::Stopped(WorkerExitObservation::Signaled {
            signal: 15,
            core_dumped: false,
        }));
        assert!(permits_teardown(&stopped));
        assert!(!describe(&stopped).contains("warning"));
        let mut refusals: Vec<Result<StopReport, String>> = [
            StopRefused::Unreachable,
            StopRefused::IdentityMismatch,
            StopRefused::AlreadyExited,
            StopRefused::Unsupported,
        ]
        .into_iter()
        .map(|refused| report(SupervisedStop::Refused(refused)))
        .collect();
        refusals.push(report(SupervisedStop::Pending));
        refusals.push(Err("no launch evidence".to_owned()));
        for refusal in &refusals {
            assert!(
                !permits_teardown(refusal),
                "{refusal:?} must not permit teardown"
            );
            let line = describe(refusal);
            assert!(line.contains("no fallback termination performed"), "{line}");
            assert!(
                !line.contains("killed"),
                "a refusal never claims a kill: {line}"
            );
        }
    }

    #[test]
    fn an_evidence_failure_is_reported_without_changing_the_decision() {
        let stopped = Ok(StopReport {
            outcome: SupervisedStop::Stopped(WorkerExitObservation::Exited { code: 0 }),
            evidence_warning: Some("disk full".to_owned()),
        });
        assert!(permits_teardown(&stopped));
        assert!(describe(&stopped).contains("warning: disk full"));
    }
}

#[cfg(unix)]
pub use imp::*;

#[cfg(unix)]
mod imp {
    use std::fs::{self, DirBuilder};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::StopReport;
    use cosmon_core::worker_exit::{
        LaunchIdentity, OwnedWorker, StopAccepted, StopRefused, WorkerControl, WorkerControlPort,
        WorkerExitObservation, WorkerExitStore, WorkerStopPort,
    };
    use serde::{Deserialize, Serialize};

    /// Version of the one-line request; another version is answered `Unsupported`.
    const PROTOCOL: u32 = 1;
    /// Largest request or reply line accepted.
    const MAX_LINE: u64 = 4096;
    /// How long the listener waits for a connected client's request line.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
    /// Interval at which an idle listener re-checks whether it is closing.
    const ACCEPT_POLL: Duration = Duration::from_millis(20);
    /// How long a client waits for a reply: the supervisor's grace plus margin.
    pub const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
    /// How long a teardown caller waits for the receipt after an accepted stop.
    pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StopRequest {
        v: u32,
        identity: LaunchIdentity,
    }

    /// Default rendezvous directory, `~/.cosmon/run/w`: short enough for the
    /// `sun_path` limit that a molecule directory exceeds on macOS.
    ///
    /// # Errors
    /// Fails when `HOME` is unset.
    pub fn default_root() -> Result<PathBuf, String> {
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join(".cosmon").join("run").join("w"))
            .ok_or_else(|| "HOME is unset; no worker control rendezvous".to_owned())
    }

    /// Rendezvous socket path for one launch under `root`.
    ///
    /// # Errors
    /// Fails only if the identity cannot be encoded.
    pub fn rendezvous(root: &Path, identity: &LaunchIdentity) -> Result<PathBuf, String> {
        Ok(root.join(format!("{}.sock", identity.rendezvous_key()?)))
    }

    fn peer_is_same_user(stream: &UnixStream) -> bool {
        peer_uid(stream).is_some_and(|uid| uid == nix::unistd::geteuid())
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn peer_uid(stream: &UnixStream) -> Option<nix::unistd::Uid> {
        nix::unistd::getpeereid(stream).ok().map(|(uid, _)| uid)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(stream: &UnixStream) -> Option<nix::unistd::Uid> {
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .ok()
            .map(|credentials| nix::unistd::Uid::from_raw(credentials.uid()))
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "linux",
        target_os = "android"
    )))]
    fn peer_uid(_stream: &UnixStream) -> Option<nix::unistd::Uid> {
        None
    }

    fn read_line(stream: &UnixStream) -> Option<String> {
        let mut line = String::new();
        BufReader::new(stream.take(MAX_LINE))
            .read_line(&mut line)
            .ok()
            .filter(|read| *read > 0)?;
        Some(line)
    }

    /// The supervisor's stop handler: admit a request only when it names this
    /// launch exactly, and let the owned child decide under its stop lock.
    ///
    /// # Errors
    /// `IdentityMismatch` for another launch; otherwise the child's refusal.
    pub fn handle_stop(
        own: &LaunchIdentity,
        requested: &LaunchIdentity,
        worker: &dyn OwnedWorker,
    ) -> Result<StopAccepted, StopRefused> {
        worker.terminate_group(&|| {
            if requested == own {
                Ok(())
            } else {
                Err(StopRefused::IdentityMismatch)
            }
        })
    }

    fn answer(stream: &UnixStream, own: &LaunchIdentity, worker: &dyn OwnedWorker) {
        if !peer_is_same_user(stream) {
            return;
        }
        let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
        let reply: Result<StopAccepted, StopRefused> = match read_line(stream)
            .and_then(|line| serde_json::from_str::<StopRequest>(&line).ok())
        {
            Some(request) if request.v == PROTOCOL => handle_stop(own, &request.identity, worker),
            _ => Err(StopRefused::Unsupported),
        };
        if let Ok(mut line) = serde_json::to_string(&reply) {
            line.push('\n');
            let mut writer = stream;
            let _ = writer.write_all(line.as_bytes());
        }
    }

    /// Unix-socket stop listener factory under one rendezvous root.
    pub struct UnixWorkerControl {
        /// Directory holding rendezvous sockets, created with mode 0700.
        pub root: PathBuf,
    }

    /// A bound listener; dropping it stops serving and unlinks its path.
    struct UnixBinding {
        path: PathBuf,
        identity: LaunchIdentity,
        listener: Option<UnixListener>,
        closing: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl WorkerControlPort for UnixWorkerControl {
        fn bind(&self, identity: &LaunchIdentity) -> Result<Box<dyn WorkerControl>, String> {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)
                .map_err(|error| {
                    format!("worker control directory unavailable ({:?})", error.kind())
                })?;
            let path = rendezvous(&self.root, identity)?;
            // The caller holds the exclusive launch claim, so a file at this path
            // belongs to a dead listener of the same identity; it is replaced,
            // never followed.
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("stale worker control socket ({:?})", error.kind()))
                }
            }
            let listener = UnixListener::bind(&path)
                .map_err(|error| format!("worker control bind failed ({:?})", error.kind()))?;
            Ok(Box::new(UnixBinding {
                path,
                identity: identity.clone(),
                listener: Some(listener),
                closing: Arc::new(AtomicBool::new(false)),
                thread: None,
            }))
        }
    }

    impl WorkerControl for UnixBinding {
        fn serve(&mut self, worker: Arc<dyn OwnedWorker>) {
            let Some(listener) = self.listener.take() else {
                return;
            };
            let identity = self.identity.clone();
            let closing = Arc::clone(&self.closing);
            // Non-blocking accept polled against `closing`, so dropping the
            // binding ends the loop even if the socket file was removed or moved.
            if listener.set_nonblocking(true).is_err() {
                return;
            }
            self.thread = Some(std::thread::spawn(move || {
                while !closing.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stream.set_nonblocking(false).is_ok() {
                                answer(&stream, &identity, worker.as_ref());
                            }
                        }
                        Err(_) => std::thread::sleep(ACCEPT_POLL),
                    }
                }
            }));
        }
    }

    impl Drop for UnixBinding {
        fn drop(&mut self) {
            self.closing.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    /// Client side of the stop channel.
    pub struct UnixWorkerStop {
        /// Directory holding rendezvous sockets.
        pub root: PathBuf,
        /// Upper bound on waiting for the supervisor's reply.
        pub reply_timeout: Duration,
    }

    impl UnixWorkerStop {
        /// Client for the default rendezvous root.
        ///
        /// # Errors
        /// Fails when `HOME` is unset.
        pub fn from_env() -> Result<Self, String> {
            Ok(Self {
                root: default_root()?,
                reply_timeout: REPLY_TIMEOUT,
            })
        }
    }

    impl WorkerStopPort for UnixWorkerStop {
        fn request_stop(&self, identity: &LaunchIdentity) -> Result<StopAccepted, StopRefused> {
            let path = rendezvous(&self.root, identity).map_err(|_| StopRefused::Unsupported)?;
            let mut stream = UnixStream::connect(path).map_err(|_| StopRefused::Unreachable)?;
            if !peer_is_same_user(&stream) {
                return Err(StopRefused::Unreachable);
            }
            let mut line = serde_json::to_string(&StopRequest {
                v: PROTOCOL,
                identity: identity.clone(),
            })
            .map_err(|_| StopRefused::Unsupported)?;
            line.push('\n');
            stream
                .write_all(line.as_bytes())
                .map_err(|_| StopRefused::Unreachable)?;
            stream
                .set_read_timeout(Some(self.reply_timeout))
                .map_err(|_| StopRefused::Unreachable)?;
            read_line(&stream)
                .and_then(|reply| {
                    serde_json::from_str::<Result<StopAccepted, StopRefused>>(&reply).ok()
                })
                .unwrap_or(Err(StopRefused::Unreachable))
        }
    }

    /// Poll a launch's receipt until it is terminal or `timeout` elapses.
    pub fn settle(receipts: &dyn WorkerExitStore, timeout: Duration) -> WorkerExitObservation {
        let deadline = Instant::now() + timeout;
        loop {
            let observation = receipts.read();
            if observation.is_terminal() || Instant::now() >= deadline {
                return observation;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stop one supervised launch before teardown and record the request.
    ///
    /// Opens exactly the launch named by `identity` in `molecule_dir`, asks its
    /// supervisor through `port` unless its receipt is already terminal, waits
    /// for the receipt after an accepted stop, and appends the request and its
    /// outcome to the launch's `stop-requests.jsonl` (evidence only).
    ///
    /// # Errors
    /// Fails when the launch evidence cannot be opened; the caller then kills
    /// nothing.
    pub fn stop_launch(
        molecule_dir: &Path,
        identity: &LaunchIdentity,
        port: &dyn WorkerStopPort,
        settle_timeout: Duration,
    ) -> Result<StopReport, String> {
        let receipts =
            cosmon_state::worker_exit::FileWorkerExitStore::open(molecule_dir, identity)?;
        let recording = Recording {
            port,
            receipts: &receipts,
            warning: std::cell::RefCell::new(None),
        };
        let outcome =
            cosmon_core::worker_exit::stop_supervised(identity, &receipts, &recording, &|| {
                settle(&receipts, settle_timeout)
            });
        Ok(StopReport {
            outcome,
            evidence_warning: recording.warning.into_inner(),
        })
    }

    struct Recording<'a> {
        port: &'a dyn WorkerStopPort,
        receipts: &'a cosmon_state::worker_exit::FileWorkerExitStore,
        warning: std::cell::RefCell<Option<String>>,
    }

    impl WorkerStopPort for Recording<'_> {
        fn request_stop(&self, identity: &LaunchIdentity) -> Result<StopAccepted, StopRefused> {
            let outcome = self.port.request_stop(identity);
            if let Err(error) = self.receipts.record_stop_request(outcome) {
                *self.warning.borrow_mut() =
                    Some(format!("stop request evidence not written: {error}"));
            }
            outcome
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicU32;

        fn identity(byte: u8) -> Result<LaunchIdentity, String> {
            Ok(cosmon_core::worker_exit::LaunchRequest::bind(
                cosmon_core::id::MoleculeId::new("task-20261019-aaaa")
                    .map_err(|error| error.to_string())?,
                cosmon_core::worker_exit::LaunchToken::from_bytes([byte; 16]),
                cosmon_core::worker_exit::LaunchToken::from_bytes([2; 16]),
                "worker",
                &[],
                Path::new("/work"),
                &[],
            )?
            .identity)
        }

        /// Records signals instead of sending them; `exited` mimics a reaped child.
        struct FakeWorker {
            exited: bool,
            signals: AtomicU32,
        }

        impl OwnedWorker for FakeWorker {
            fn pid(&self) -> u32 {
                456
            }
            fn wait(&self) -> Result<WorkerExitObservation, String> {
                Ok(WorkerExitObservation::Unknown)
            }
            fn terminate_group(
                &self,
                authorize: &dyn Fn() -> Result<(), StopRefused>,
            ) -> Result<StopAccepted, StopRefused> {
                authorize()?;
                if self.exited {
                    return Err(StopRefused::AlreadyExited);
                }
                self.signals.fetch_add(1, Ordering::SeqCst);
                Ok(StopAccepted { escalated: false })
            }
        }

        fn fake(exited: bool) -> FakeWorker {
            FakeWorker {
                exited,
                signals: AtomicU32::new(0),
            }
        }

        #[test]
        fn a_matching_identity_signals_and_a_different_one_is_refused() -> Result<(), String> {
            let own = identity(1)?;
            let worker = fake(false);
            assert_eq!(
                handle_stop(&own, &identity(9)?, &worker),
                Err(StopRefused::IdentityMismatch)
            );
            assert_eq!(worker.signals.load(Ordering::SeqCst), 0);
            assert_eq!(
                handle_stop(&own, &own, &worker),
                Ok(StopAccepted { escalated: false })
            );
            assert_eq!(worker.signals.load(Ordering::SeqCst), 1);
            Ok(())
        }

        #[test]
        fn a_stop_after_exit_is_refused_without_a_signal() -> Result<(), String> {
            let own = identity(1)?;
            let worker = fake(true);
            assert_eq!(
                handle_stop(&own, &own, &worker),
                Err(StopRefused::AlreadyExited)
            );
            assert_eq!(worker.signals.load(Ordering::SeqCst), 0);
            Ok(())
        }

        #[test]
        fn the_socket_round_trip_carries_identity_and_refusal() -> Result<(), String> {
            let root = tempfile::tempdir().map_err(|error| error.to_string())?;
            let own = identity(1)?;
            let control = UnixWorkerControl {
                root: root.path().join("w"),
            };
            let mut binding = control.bind(&own)?;
            let worker: Arc<FakeWorker> = Arc::new(fake(false));
            binding.serve(worker.clone());
            let client = UnixWorkerStop {
                root: root.path().join("w"),
                reply_timeout: Duration::from_secs(5),
            };
            assert_eq!(
                client.request_stop(&identity(9)?),
                Err(StopRefused::Unreachable)
            );
            assert_eq!(worker.signals.load(Ordering::SeqCst), 0);
            assert_eq!(
                client.request_stop(&own),
                Ok(StopAccepted { escalated: false })
            );
            assert_eq!(worker.signals.load(Ordering::SeqCst), 1);
            let path = rendezvous(&root.path().join("w"), &own)?;
            assert!(path.exists());
            drop(binding);
            assert!(!path.exists());
            assert_eq!(client.request_stop(&own), Err(StopRefused::Unreachable));
            Ok(())
        }

        #[test]
        fn a_squatter_with_another_identity_at_the_same_path_is_refused() -> Result<(), String> {
            // The path is only a rendezvous: a listener bound for launch A but
            // reached for launch B (forced onto A's path) refuses B.
            let root = tempfile::tempdir().map_err(|error| error.to_string())?;
            let (a, b) = (identity(1)?, identity(9)?);
            let control = UnixWorkerControl {
                root: root.path().to_owned(),
            };
            let mut binding = control.bind(&a)?;
            let worker: Arc<FakeWorker> = Arc::new(fake(false));
            binding.serve(worker.clone());
            fs::rename(rendezvous(root.path(), &a)?, rendezvous(root.path(), &b)?)
                .map_err(|error| error.to_string())?;
            let client = UnixWorkerStop {
                root: root.path().to_owned(),
                reply_timeout: Duration::from_secs(5),
            };
            assert_eq!(client.request_stop(&b), Err(StopRefused::IdentityMismatch));
            assert_eq!(worker.signals.load(Ordering::SeqCst), 0);
            Ok(())
        }
    }
}
