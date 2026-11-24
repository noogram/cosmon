// SPDX-License-Identifier: AGPL-3.0-only

//! `cs gates slot` and the runner around it (ADR-190 D1, noogram/cosmon#214),
//! driven through the real binary: the process that holds a slot in production
//! is `cs gates slot`, so a crash, an exit code or an overlap is only
//! meaningful when it is that process.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_filestore::gate_slots::live_gate_slot_holders_in;
use nix::sys::signal::{kill, killpg, Signal};
use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
use nix::unistd::{getpgid, Pid};

/// A galaxy under `tmp` whose config declares `gate_slots_toml`; returns the
/// state directory.
fn galaxy(tmp: &Path, gate_slots_toml: &str) -> PathBuf {
    let cosmon = tmp.join(".cosmon");
    let state = cosmon.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        format!("[project]\nproject_id = \"gate-slots-cli\"\n\n{gate_slots_toml}\n"),
    )
    .unwrap();
    state
}

/// A hermetic `cs gates slot …` against `state`.
fn cs(state: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    for k in [
        "COSMON_PARENT_MOL_ID",
        "COSMON_MOL_DIR",
        "COSMON_SESSION_ID",
        "CB_DEPTH",
        "COSMON_EGRESS_POLICY",
    ] {
        cmd.env_remove(k);
    }
    cmd.env("COSMON_STATE_DIR", state).current_dir(state);
    cmd
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    wait_until_within(what, Duration::from_secs(60), &mut cond);
}

fn wait_until_within(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start `cs gates slot` in its own process group so its command can be reaped.
fn spawn_slot(state: &Path, pool: &str, command: &[&str]) -> Child {
    let mut cmd = cs(state);
    cmd.args(["gates", "slot", "--pool", pool, "--"])
        .args(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    cmd.spawn().unwrap()
}

fn pid(id: u32) -> Pid {
    Pid::from_raw(i32::try_from(id).unwrap())
}

/// Keep a handle to both the wrapper and its command group for cleanup, even
/// when an assertion exposes a surviving command on the unmodified binary.
struct RunningSlot {
    wrapper: Child,
    command: Pid,
    group: Pid,
}

impl RunningSlot {
    fn start(state: &Path, options: &[&str], script: &str) -> Self {
        let marker = state.join("command.pid");
        let mut wrapper = cs(state)
            .args(["gates", "slot"])
            .args(options)
            .args(["--", "sh", "-c", script, "sh"])
            .arg(&marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let start = Instant::now();
        let command = loop {
            if let Ok(contents) = fs::read_to_string(&marker) {
                if let Ok(id) = contents.trim().parse() {
                    break pid(id);
                }
            }
            assert_eq!(wrapper.try_wait().unwrap(), None, "command never started");
            if start.elapsed() >= Duration::from_secs(10) {
                let _ = wrapper.kill();
                let _ = wrapper.wait();
                panic!("timed out waiting for the command PID");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let group = getpgid(Some(command)).unwrap();
        Self {
            wrapper,
            command,
            group,
        }
    }

    fn kill_wrapper(&mut self) {
        self.wrapper.kill().unwrap();
        self.wrapper.wait().unwrap();
    }
}

impl Drop for RunningSlot {
    fn drop(&mut self) {
        // The baseline still puts the command in the wrapper's group.
        let _ = killpg(self.group, Signal::SIGKILL);
        let _ = self.wrapper.kill();
        let _ = self.wrapper.wait();
    }
}

fn process_is_alive(process: Pid) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &process.to_string()])
        .output()
        .unwrap();
    // Orphaned zombies may await a reaper, but cannot keep running.
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .trim_start()
            .starts_with(|state: char| state != 'Z')
}

#[test]
fn the_command_can_read_the_terminal_and_returns_it_to_the_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    let pty = nix::pty::openpty(None, None).unwrap();
    // No reader drains the master. An echo left queued there can hold up
    // session teardown on macOS even after both reads have succeeded.
    let mut mode = tcgetattr(&pty.slave).unwrap();
    mode.local_flags.remove(LocalFlags::ECHO);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &mode).unwrap();
    let mut terminal = File::from(pty.master.try_clone().unwrap());
    let slave = pty.slave.try_clone().unwrap();
    drop(pty);
    let mut caller = Command::new("sh");
    caller
        .current_dir(&state)
        .env("COSMON_STATE_DIR", &state)
        .args([
            "-c",
            r#""$1" gates slot -- sh -c 'read input; echo "$input" > child-input' &&
                read input && echo "$input" > caller-input"#,
            "sh",
            env!("CARGO_BIN_EXE_cs"),
        ])
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: these syscalls create a session and attach the already-set-up
    // stdin as its controlling terminal. The pre-exec hook allocates nothing.
    unsafe {
        caller.pre_exec(|| {
            if libc::setsid() == -1
                || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1
                || libc::tcsetpgrp(0, libc::getpgrp()) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut caller = caller.spawn().unwrap();
    terminal.write_all(b"for child\n").unwrap();
    let start = Instant::now();
    let mut sent_to_caller = false;
    let status = loop {
        if let Some(status) = caller.try_wait().unwrap() {
            break status;
        }
        if !sent_to_caller && state.join("child-input").exists() {
            terminal.write_all(b"for caller\n").unwrap();
            sent_to_caller = true;
        }
        if start.elapsed() > Duration::from_secs(5) {
            drop(terminal);
            let _ = killpg(pid(caller.id()), Signal::SIGKILL);
            caller.wait().unwrap();
            panic!(
                "the gate caller could not finish reading its terminal (child read: {})",
                sent_to_caller
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
    assert_eq!(
        fs::read_to_string(state.join("child-input")).unwrap(),
        "for child\n"
    );
    assert_eq!(
        fs::read_to_string(state.join("caller-input")).unwrap(),
        "for caller\n"
    );
}

#[test]
fn sending_a_signal_to_the_wrapper_stops_the_whole_command_group() {
    for (signal, number) in [
        (Signal::SIGTERM, 15),
        (Signal::SIGINT, 2),
        (Signal::SIGHUP, 1),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
        let script = format!(
            "trap 'echo caught > signal; exit 7' {number}; \
             sh -c 'trap \"\" {number}; exec sleep 300' & \
             echo $! > descendant.pid; echo $$ > \"$1\"; wait"
        );
        let mut run = RunningSlot::start(&state, &[], &script);
        let descendant = pid(fs::read_to_string(state.join("descendant.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap());
        kill(pid(run.wrapper.id()), signal).unwrap();
        wait_until_within("signalled wrapper to exit", Duration::from_secs(5), || {
            run.wrapper.try_wait().unwrap().is_some()
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !process_is_alive(run.command),
            "{signal} left the command alive"
        );
        assert!(
            !process_is_alive(descendant),
            "{signal} left its descendant alive"
        );
        assert_eq!(
            fs::read_to_string(state.join("signal")).unwrap(),
            "caught\n"
        );
        assert_eq!(run.group, run.command, "the command leads its group");
    }
}

#[test]
fn timeout_kills_the_group_and_publishes_a_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    let mut run = RunningSlot::start(
        &state,
        &["--timeout", "1", "--verdict-file", "verdict"],
        "sleep 30 & echo $! > descendant.pid; echo $$ > \"$1\"; wait",
    );
    let descendant = pid(fs::read_to_string(state.join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap());
    wait_until_within("timed-out wrapper to exit", Duration::from_secs(5), || {
        run.wrapper.try_wait().unwrap().is_some()
    });
    assert_eq!(run.wrapper.wait().unwrap().code(), Some(124));
    assert_eq!(
        fs::read_to_string(state.join("verdict")).unwrap(),
        "timed_out"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(!process_is_alive(run.command));
    assert!(!process_is_alive(descendant));
}

#[test]
fn the_timeout_excludes_time_queued_for_a_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    let first = RunningSlot::start(&state, &[], "echo $$ > \"$1\"; exec sleep 300");
    fs::write(state.join("verdict"), "ran:7").unwrap();
    let mut second = cs(&state)
        .args([
            "gates",
            "slot",
            "--timeout",
            "1",
            "--verdict-file",
            "verdict",
            "--",
            "true",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = std::io::BufReader::new(second.stderr.take().unwrap());
    wait_for_queue(&mut stderr);
    assert!(
        !state.join("verdict").exists(),
        "stale verdict while queued"
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert!(second.try_wait().unwrap().is_none());
    drop(first);
    assert!(second.wait().unwrap().success());
    assert_eq!(fs::read_to_string(state.join("verdict")).unwrap(), "ran:0");
}

fn wait_for_queue(stderr: &mut impl std::io::BufRead) {
    loop {
        let mut line = String::new();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0, "no queue notice");
        if line.contains("queued for a slot") {
            return;
        }
    }
}

#[test]
fn required_directory_is_probed_before_and_after_the_command() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    fs::write(state.join("file"), "").unwrap();
    fs::create_dir(state.join("empty")).unwrap();
    for (path, script, ran) in [
        ("missing", "touch ran", false),
        ("file", "touch ran", false),
        ("empty", "touch ran; rmdir empty; exit 7", true),
    ] {
        let status = cs(&state)
            .args([
                "gates",
                "slot",
                "--require-dir",
                path,
                "--verdict-file",
                "verdict",
                "--",
                "sh",
                "-c",
                script,
            ])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(125));
        let verdict = fs::read_to_string(state.join("verdict")).unwrap();
        let reason = verdict.strip_prefix("could_not_run:").unwrap();
        assert!(!reason.is_empty() && !reason.contains('\n'), "{verdict:?}");
        assert_eq!(state.join("ran").exists(), ran, "required path {path}");
    }
}

#[test]
fn a_command_that_cannot_start_has_a_could_not_run_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    let status = cs(&state)
        .args([
            "gates",
            "slot",
            "--verdict-file",
            "verdict",
            "--",
            "./absent",
        ])
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(fs::read_to_string(state.join("verdict"))
        .unwrap()
        .starts_with("could_not_run:"));
    let legacy = cs(&state)
        .args(["gates", "slot", "--", "./absent"])
        .output()
        .unwrap();
    assert_eq!(legacy.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("cannot run `./absent`"));
}

#[test]
fn an_existing_directory_cannot_be_used_as_a_verdict_file() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    fs::create_dir(state.join("verdict")).unwrap();
    let status = cs(&state)
        .args([
            "gates",
            "slot",
            "--verdict-file",
            "verdict",
            "--",
            "touch",
            "ran",
        ])
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!state.join("ran").exists());
    assert!(state.join("verdict").is_dir());
}

#[test]
fn a_verdict_can_be_published_outside_a_read_only_working_directory() {
    if nix::unistd::geteuid().is_root() {
        return; // Root can write through the fixture's permission bits.
    }
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    let workdir = tmp.path().join("work");
    fs::create_dir(&workdir).unwrap();
    fs::set_permissions(&workdir, fs::Permissions::from_mode(0o555)).unwrap();
    let verdict = tmp.path().join("verdict");

    let output = cs(&state)
        .current_dir(&workdir)
        .args(["gates", "slot", "--verdict-file"])
        .arg(&verdict)
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(verdict).unwrap(), "ran:0");
}

#[test]
fn verdict_preserves_the_command_exit_code_with_a_usable_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    for code in [0, 7, 124, 125] {
        let status = cs(&state)
            .args([
                "gates",
                "slot",
                "--require-dir",
                ".",
                "--verdict-file",
                "verdict",
                "--",
                "sh",
                "-c",
                &format!("exit {code}"),
            ])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        assert_eq!(
            fs::read_to_string(state.join("verdict")).unwrap(),
            format!("ran:{code}")
        );
    }
}

#[test]
fn descriptor_handoff_keeps_a_controller_slot_held_after_wrapper_sigkill() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots.pools]\ncontroller = 1\n");
    let mut first = RunningSlot::start(
        &state,
        &["--pool", "controller", "--hand-descriptor"],
        "sleep 300 & echo $! > descendant.pid; echo $$ > \"$1\"; wait",
    );
    let descendant = pid(fs::read_to_string(state.join("descendant.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap());
    first.kill_wrapper();
    kill(first.command, Signal::SIGKILL).unwrap();
    wait_until("the first controller to die", || {
        !process_is_alive(first.command)
    });
    assert!(process_is_alive(descendant));
    assert_eq!(live_gate_slot_holders_in(&state, "controller").unwrap(), 1);

    let marker = state.join("second-ran");
    let mut second = cs(&state)
        .args([
            "gates",
            "slot",
            "--pool",
            "controller",
            "--",
            "touch",
            "second-ran",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = std::io::BufReader::new(second.stderr.take().unwrap());
    wait_for_queue(&mut stderr);
    assert!(!marker.exists(), "the second controller raced the orphan");
    kill(descendant, Signal::SIGKILL).unwrap();
    wait_until("second controller to run after the orphan died", || {
        marker.exists()
    });
    assert!(second.wait().unwrap().success());
}

#[test]
fn descriptor_handoff_requires_a_named_bounded_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    for pool in ["full_gate", "undeclared"] {
        let output = cs(&state)
            .args([
                "gates",
                "slot",
                "--pool",
                pool,
                "--hand-descriptor",
                "--",
                "touch",
                "ran",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("bounded named pool"));
        assert!(!state.join("ran").exists());
    }
}

/// The command's own exit code is the verb's exit code.
#[test]
fn the_verb_returns_the_commands_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    for code in [0, 7] {
        let status = cs(&state)
            .args(["gates", "slot", "--", "sh", "-c", &format!("exit {code}")])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
    }

    let status = cs(&state)
        .args(["gates", "slot", "--", "sh", "-c", "kill -TERM $$"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(143), "128 + SIGTERM");
}

/// `kill -9` of the holder while another run waits: the waiter starts. The
/// slot is the kernel's flock, so nothing has to clean up after the dead holder.
#[test]
fn a_killed_holder_frees_its_slot_for_the_waiter() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    let marker = tmp.path().join("waiter-ran");

    let mut holder = RunningSlot::start(&state, &[], "echo $$ > \"$1\"; exec sleep 300");
    wait_until("the holder to take the slot", || {
        live_gate_slot_holders_in(&state, "full_gate").unwrap() == 1
    });
    let mut waiter = spawn_slot(&state, "full_gate", &["touch", marker.to_str().unwrap()]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(!marker.exists(), "the waiter ran while the slot was held");
    assert!(waiter.try_wait().unwrap().is_none());

    // SIGKILL of the holder alone: no destructor runs, only the kernel releases
    // the lock. Its orphaned `sleep` stays alive for now and must not pin the
    // slot (the descriptor is close-on-exec), so the waiter starts regardless.
    holder.kill_wrapper();
    wait_until("the waiter to run", || marker.exists());
    assert!(waiter.wait().unwrap().success());
    assert!(process_is_alive(holder.command));
}

/// N=2 and five concurrent runs: the log of starts and ends never shows more
/// than two inside at once.
#[test]
fn concurrent_runs_through_the_verb_never_exceed_the_pool_size() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 2\n");
    let log = tmp.path().join("overlap.log");
    let script = format!(
        "echo + >> '{log}'; sleep 0.4; echo - >> '{log}'",
        log = log.display()
    );
    let runs: Vec<Child> = (0..5)
        .map(|_| spawn_slot(&state, "full_gate", &["sh", "-c", &script]))
        .collect();
    for mut r in runs {
        assert!(r.wait().unwrap().success());
    }
    let (mut now, mut peak) = (0_i32, 0_i32);
    for line in fs::read_to_string(&log).unwrap().lines() {
        now += if line == "+" { 1 } else { -1 };
        peak = peak.max(now);
    }
    assert_eq!(peak, 2, "peak overlap");
}

/// An unconfigured pool is unbounded: the command runs at once, and
/// `--configured` says so with exit 3 without running anything.
#[test]
fn an_unconfigured_pool_runs_at_once_and_reports_itself_unbounded() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    let ran = tmp.path().join("ran");
    let status = cs(&state)
        .args(["gates", "slot", "--", "touch", ran.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success() && ran.exists());
    assert!(!state.join("gate-slots").exists(), "a slot was taken");
    let configured = cs(&state)
        .args(["gates", "slot", "--configured"])
        .status()
        .unwrap();
    assert_eq!(configured.code(), Some(3));
}

/// A named pool is independent of `full_gate`: holders of one never block the
/// other, and each reads its own size.
#[test]
fn named_pools_have_their_own_size_and_do_not_block_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(
        tmp.path(),
        "[gate_slots]\nfull_gate = 1\n[gate_slots.pools]\ndocker_build = 3\n",
    );
    let out = cs(&state)
        .args(["gates", "slot", "--pool", "docker_build", "--configured"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3");

    let full = RunningSlot::start(&state, &[], "echo $$ > \"$1\"; exec sleep 300");
    wait_until("full_gate to be held", || {
        live_gate_slot_holders_in(&state, "full_gate").unwrap() == 1
    });
    let status = cs(&state)
        .args(["gates", "slot", "--pool", "docker_build", "--", "true"])
        .status()
        .unwrap();
    assert!(status.success(), "docker_build waited for full_gate");
    drop(full);
}

fn runner() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/run-gates-bg.sh")
}

fn run_runner(state: &Path, out: &Path, command: &[&str]) {
    let mut cmd = Command::new("bash");
    cmd.arg(runner())
        .args(command)
        .env("COSMON_STATE_DIR", state)
        .env("COSMON_RUN_GATES_BG_CS", env!("CARGO_BIN_EXE_cs"))
        .env("COSMON_RUN_GATES_BG_DIR", out)
        .current_dir(state)
        .stdout(Stdio::null());
    assert!(cmd.status().unwrap().success());
}

/// The runner with the real verb, N=1: while another run holds the slot it
/// writes `gates.queued`, no `gates.pid`, no `gates.exit`; once the slot frees
/// the bundle runs, `gates.pid` replaces `gates.queued`, and `gates.exit`
/// follows the bundle.
#[test]
fn the_runner_queues_behind_a_held_slot_and_then_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "[gate_slots]\nfull_gate = 1\n");
    let out = tmp.path().join("run");
    fs::create_dir_all(&out).unwrap();
    let ran = tmp.path().join("bundle-ran");

    let mut holder = RunningSlot::start(&state, &[], "echo $$ > \"$1\"; exec sleep 300");
    wait_until("the slot to be held", || {
        live_gate_slot_holders_in(&state, "full_gate").unwrap() == 1
    });
    run_runner(&state, &out, &["touch", ran.to_str().unwrap()]);
    assert!(out.join("gates.queued").exists());
    std::thread::sleep(Duration::from_millis(600));
    assert!(!out.join("gates.pid").exists() && !out.join("gates.exit").exists());
    assert!(!ran.exists());

    holder.kill_wrapper();
    wait_until("gates.exit", || out.join("gates.exit").exists());
    assert!(ran.exists(), "gates.exit appeared before the bundle ran");
    assert_eq!(fs::read_to_string(out.join("gates.exit")).unwrap(), "0");
    assert!(out.join("gates.pid").exists() && !out.join("gates.queued").exists());
}

/// Unset config: the runner writes the files it always wrote, no
/// `gates.queued`, and takes no slot.
#[test]
fn the_runner_without_gate_slots_writes_no_queued_file_and_takes_no_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let state = galaxy(tmp.path(), "");
    let out = tmp.path().join("run");
    fs::create_dir_all(&out).unwrap();
    run_runner(&state, &out, &["sh", "-c", "sleep 0.3"]);
    assert!(out.join("gates.pid").exists());
    assert!(!out.join("gates.queued").exists());
    wait_until("gates.exit", || out.join("gates.exit").exists());
    assert!(!out.join("gates.queued").exists());
    assert!(!state.join("gate-slots").exists(), "a slot was taken");
}

/// `cs done` never takes a slot: no landing path refers to the slot primitive,
/// so a landing is never queued behind a gate (ADR-190 §3, lock order
/// slot → trunk → fleet). Scans the sources; the primitive may appear only in
/// the files that own or observe it.
#[test]
fn no_cs_done_path_acquires_a_gate_slot() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let allowed = [
        "cosmon-filestore/src/gate_slots.rs",
        "cosmon-cli/src/cmd/gates.rs",
        "cosmon-cli/src/cmd/patrol_background.rs",
        // Test-only: patrol's verdict tests hold a slot to stage a busy pool.
        "cosmon-cli/src/cmd/patrol.rs",
    ];
    let mut offenders = Vec::new();
    let mut stack = vec![crates.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name != "tests" && name != "target" && name != "examples" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&crates)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let src = fs::read_to_string(&path).unwrap_or_default();
                let takes_slot = ["acquire_gate_slot", "gate_slots::", "gates slot"]
                    .iter()
                    .any(|needle| src.contains(needle));
                // Comments and docs may name the primitive; code may not call it.
                let in_code = src.lines().any(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//")
                        && ["acquire_gate_slot", "gate_slots::"]
                            .iter()
                            .any(|n| t.contains(n))
                });
                if takes_slot && in_code && !allowed.iter().any(|a| rel.ends_with(a)) {
                    offenders.push(rel);
                }
            }
        }
    }
    // The cosmon-filestore lib re-declares the module; that line is the only
    // other legitimate mention.
    offenders.retain(|o| !o.ends_with("cosmon-filestore/src/lib.rs"));
    assert!(
        offenders.is_empty(),
        "slot primitive used outside its owners: {offenders:?}"
    );
}
