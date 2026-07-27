// SPDX-License-Identifier: AGPL-3.0-only

//! The first-run consent question must never be able to hang a run.
//!
//! # What this file pins, and why a consent-file assertion would not
//!
//! On 2026-07-27 a tester's container dispatch hung for its full 240s timeout
//! and spawned nothing. The captured output was the French `opt-in-share`
//! prompt: `cs tackle` had asked a question through a stdout the orchestrator
//! was capturing (`OUT="$(cs tackle …)"`), on a stdin that was still the
//! inherited terminal from `docker exec -it`. No keystroke could arrive; no
//! output could warn. The guard was `stdin().is_terminal()`, which answers
//! *"is a terminal attached?"* — not *"can a human see this and answer it?"*.
//!
//! The tests below therefore assert the **non-blocking property**, by running
//! the real binary against a real pty and requiring it to *terminate*. A test
//! that merely inspected `consent.toml` afterwards would pass against the
//! broken build too: the broken build also writes a declined record — it just
//! writes it after somebody types into a terminal nobody is watching, or
//! never. Only "the process exited on its own, within a deadline" separates
//! the two builds.
//!
//! Unix-only: the failure mode is a pty, and there is no pty to allocate on
//! the platforms without one.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long a non-blocking command is allowed to take before we call it hung.
///
/// Generous on purpose: a false "hung" verdict from a loaded CI box would be
/// worse than a slow test, and the property under test is *terminates at all*,
/// not *terminates fast*. The container failure blocked indefinitely; 30s is
/// far beyond any legitimate run of these two commands.
const DEADLINE: Duration = Duration::from_secs(30);

fn cs_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cs"))
}

/// A freshly allocated pseudo-terminal pair.
///
/// The master end is held by the test for as long as the child runs — that is
/// what makes the child's stdin a genuine, *open* terminal rather than one
/// that would conveniently hit EOF and unblock a `read_line` on its own. A
/// pty that EOFs is not the situation that hung the container.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    fn open() -> std::io::Result<Self> {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        // SAFETY: both out-parameters are valid, exclusively-borrowed ints;
        // the three optional pointers are passed null, which `openpty`
        // documents as "use the defaults". On success the two fds are owned
        // by this process and handed straight to `OwnedFd`.
        let rc = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `openpty` returned 0, so both fds are freshly opened and
        // owned by us; neither is duplicated elsewhere.
        unsafe {
            Ok(Self {
                master: OwnedFd::from_raw_fd(master),
                slave: OwnedFd::from_raw_fd(slave),
            })
        }
    }

    /// A duplicate of the slave end, suitable for a child's stdio slot.
    fn slave_stdio(&self) -> std::io::Result<Stdio> {
        Ok(Stdio::from(self.slave.try_clone()?))
    }
}

/// Wait for `child`, failing the test with `what` if it outlives [`DEADLINE`].
///
/// Polling rather than blocking on `wait()` is the whole point: a blocking
/// wait against a hung child *is* the bug, reproduced inside the test harness.
fn wait_or_hang(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{what} did not terminate within {DEADLINE:?} — it is blocked on a question \
                     nobody can answer (stdin is a tty, stdout is captured)"
                );
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn read_to_string(mut r: impl Read) -> String {
    let mut buf = Vec::new();
    // A pty master reports EIO (not EOF) on Linux once the last slave closes;
    // whatever was read before that is the output we care about.
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The exact geometry of the container hang: a terminal on stdin, a captured
/// stdout. `cs opt-in-share` must decline itself and exit instead of asking.
#[test]
fn consent_with_tty_stdin_and_captured_stdout_does_not_block() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let pty = Pty::open().expect("openpty");

    let mut child = cs_bin()
        .env("COSMON_CONFIG_HOME", tmp.path())
        .arg("opt-in-share")
        .stdin(pty.slave_stdio().expect("dup slave"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cs opt-in-share");

    let status = wait_or_hang(&mut child, "cs opt-in-share");
    let stdout = read_to_string(child.stdout.take().expect("stdout pipe"));
    let stderr = read_to_string(child.stderr.take().expect("stderr pipe"));
    drop(pty);

    assert!(
        status.success(),
        "cs opt-in-share should exit 0; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Acceptez-vous"),
        "the question must not be printed into a stdout nobody reads:\n{stdout}"
    );
    // The auto-decline is not silent: stdout was captured, so the trace goes
    // to stderr, naming the explicit remedy.
    assert!(
        stderr.contains("opt-in-share") && stderr.contains("cs opt-in-share --accept"),
        "auto-decline must leave a trace on stderr, got:\n{stderr}"
    );
    let body = std::fs::read_to_string(tmp.path().join("cosmon/consent.toml"))
        .expect("consent.toml written");
    assert!(body.contains("declined_at"), "expected a decline:\n{body}");
}

/// The dispatch path asks nothing at all — not even on a fully interactive
/// terminal, where the old hook *would* have prompted.
///
/// Run from a directory with no galaxy, so `cs tackle` fails fast on project
/// identity. The assertion is that it reaches that failure: no consent record
/// is created, no question is printed, and the run terminates. Both stdio ends
/// are the pty, which is the friendliest possible case for a prompt — if a
/// question survives anywhere on this path, it survives here.
#[test]
fn tackle_never_asks_a_consent_question() {
    let cwd = tempfile::TempDir::new().expect("cwd tempdir");
    let cfg = tempfile::TempDir::new().expect("config tempdir");
    let pty = Pty::open().expect("openpty");

    // Drain the master end while the child runs: a pty whose buffer fills
    // stops the writer, which would be a hang of our own making rather than
    // the one under test.
    let master = pty.master.try_clone().expect("dup master");
    let drain = std::thread::spawn(move || read_to_string(std::fs::File::from(master)));

    let mut child = cs_bin()
        .current_dir(cwd.path())
        .env("COSMON_CONFIG_HOME", cfg.path())
        .args(["tackle", "task-does-not-exist"])
        .stdin(pty.slave_stdio().expect("dup slave"))
        .stdout(pty.slave_stdio().expect("dup slave"))
        .stderr(pty.slave_stdio().expect("dup slave"))
        .spawn()
        .expect("spawn cs tackle");

    let _status = wait_or_hang(&mut child, "cs tackle");
    drop(pty);
    let transcript = drain.join().unwrap_or_default();

    assert!(
        !transcript.contains("Acceptez-vous"),
        "cs tackle must not ask the consent question:\n{transcript}"
    );
    assert!(
        !Path::new(&cfg.path().join("cosmon/consent.toml")).exists(),
        "cs tackle must not record a consent decision — the question does not live there"
    );
}
