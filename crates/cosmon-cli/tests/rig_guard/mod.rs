// SPDX-License-Identifier: AGPL-3.0-only

//! RAII teardown for integration rigs that own out-of-process state.
//!
//! Lives under `tests/rig_guard/` (a *subdirectory*, not a top-level
//! `tests/rig_guard.rs`) so Cargo treats it as a module to `mod`-include from a
//! test binary rather than compiling it as a test binary of its own. A test
//! that needs it declares `mod rig_guard;`.
//!
//! # Why this exists
//!
//! A rig built out of `tmux new-session` and a deliberately-orphaned child is
//! not owned by the Rust value graph: nothing in it is freed when a test's
//! stack unwinds. So a teardown written as the *last statements of the test*
//! runs on exactly one path — the one where every assertion passed. The panic
//! path, which is the path a failing test takes, skips it and leaves a tmux
//! server and a detached process behind on the machine.
//!
//! That was not hypothetical. The 2026-07-31 CI incident had
//! `briefing_backstop_survival` spending its full 60 s patience on a tautological
//! wait and then panicking past its `tmux kill-server` — every run leaking a
//! server *and* the detached backstop it had just armed, on a machine where the
//! next run's `pgrep`-shaped checks then had company.
//!
//! The fix is the ordinary Rust one: give the out-of-process thing an owner.
//! Each type here holds one kind of external resource and releases it in
//! [`Drop`], which unwinding runs and a `return` runs and a passing test runs.
//! Teardown stops being a step a test can forget.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

/// A tmux server named by its socket, killed when this value is dropped.
///
/// Owns the *server*, not a session: `kill-server` on a private `-L` socket is
/// the only teardown that is total (a session can outlive `kill-session` by
/// spawning another, and a pane's `sleep 300` outlives nothing else).
pub struct TmuxServer {
    socket: String,
}

impl TmuxServer {
    /// Claim `socket` as this value's to tear down.
    ///
    /// Constructed *before* the first `new-session`, deliberately: a
    /// `new-session` that half-succeeds — server up, session command bad — still
    /// leaves a server, and a guard created after the assertion on its exit
    /// status would never exist to remove it.
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Run `tmux -L <socket> <args…>` and hand back what it said.
    pub fn run(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux must be runnable")
    }

    /// Does a session named `session` exist on this socket right now?
    ///
    /// `has-session` and not a `list-sessions` grep: once the server is gone the
    /// command fails outright, which is the same answer as "no such session" and
    /// is exactly what a teardown assertion wants.
    pub fn has_session(&self, session: &str) -> bool {
        Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(["has-session", "-t", session])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Where this socket lives on disk, if a server is currently listening on it.
    ///
    /// Asked of tmux rather than derived: the directory is
    /// `$TMUX_TMPDIR`-dependent and the fallback differs by platform, and a
    /// reimplementation that guessed wrong would silently point at nothing.
    pub fn socket_path(&self) -> Option<PathBuf> {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(["display-message", "-p", "#{socket_path}"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // Read while the server is still up: once it is gone, tmux can no longer
        // be asked where its socket was.
        let socket_path = self.socket_path();

        // Best-effort by construction: the server may already be gone (the happy
        // path may have killed it, or the test never got as far as starting it),
        // and a `Drop` that panicked while unwinding would abort the process and
        // replace a readable test failure with one nobody can diagnose.
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // `kill-server` ends the server but does *not* unlink its socket —
        // measured on macOS, 2026-07-31: the `srw-rw----` entry survives the
        // server by design, since tmux uses the file's presence to find an
        // existing server rather than as a liveness record. A rig that names its
        // socket after its pid therefore leaves one dead inode per run, and the
        // developer machine that prompted this molecule had accumulated several
        // hundred of them under `/tmp/tmux-501/`. Nothing else will ever collect
        // them, so the owner does.
        if let Some(path) = socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A spawned [`Child`] that is killed and reaped when this value is dropped.
///
/// `std::process::Child` deliberately does *not* do this — dropping it detaches
/// the process — so every child a test holds across an assertion is a potential
/// orphan. Wrapping it moves the process into the value graph.
pub struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    /// Take ownership of an already-spawned child.
    pub fn new(child: Child) -> Self {
        Self { child }
    }

    /// The child's pid.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the child and return its status, as [`Child::wait`] would.
    ///
    /// Calling this and *then* letting the guard drop is the normal shape:
    /// `Child` caches the status, so the drop below is a no-op rather than a
    /// second reap.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Both are expected to fail on the happy path: `kill` answers
        // `InvalidInput` for a process that already exited, and `wait` returns
        // the cached status. Neither is a teardown failure.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every process whose command line carries `marker`, killed on drop.
///
/// The one kind of leak a [`ChildGuard`] cannot cover: a process that was
/// *deliberately* orphaned, so no `Child` handle to it exists anywhere. The
/// briefing backstop is spawned by a caller that then dies; the test never
/// learns its pid. What the test does know is the unique path it passed on the
/// command line, and a command line is readable from the process table.
///
/// `marker` must be unique to the run — a temp-directory path is, a subcommand
/// name is not.
pub struct DetachedByArgv {
    marker: String,
}

impl DetachedByArgv {
    /// Watch for processes whose argv contains `marker`.
    pub fn watching(marker: impl Into<String>) -> Self {
        Self {
            marker: marker.into(),
        }
    }

    /// The pids currently matching, newest state of the process table.
    ///
    /// Reads `ps` rather than `pgrep`: `pgrep -f` differs between BSD and procps
    /// in what it matches and how it reports "nothing found", and this file has
    /// already been bitten once by a `kill(1)` whose argument parsing differed
    /// across the same two platforms.
    pub fn pids(&self) -> Vec<i32> {
        let me = std::process::id();
        let Ok(out) = Command::new("ps")
            .args(["-Aww", "-o", "pid=,args="])
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let (pid, argv) = line.split_once(char::is_whitespace)?;
                let pid: i32 = pid.parse().ok()?;
                // Never name ourselves: the test binary's own argv does not
                // carry the marker, but a future caller's could.
                (pid != me as i32 && argv.contains(&self.marker)).then_some(pid)
            })
            .collect()
    }
}

impl Drop for DetachedByArgv {
    fn drop(&mut self) {
        for pid in self.pids() {
            // The positive pid and not `-pid`: a matched process is not
            // guaranteed to lead its group, and a negative pid that names some
            // *other* group would kill processes this test never spawned.
            // SAFETY: a plain `kill(2)` on a pid read from the process table; no
            // memory is touched. A pid that has since exited answers ESRCH,
            // which is ignored like every other teardown failure.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}
