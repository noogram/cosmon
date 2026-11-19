// SPDX-License-Identifier: AGPL-3.0-only

//! `cs gates slot` and the runner around it (ADR-190 D1, noogram/cosmon#214),
//! driven through the real binary: the process that holds a slot in production
//! is `cs gates slot`, so a crash, an exit code or an overlap is only
//! meaningful when it is that process.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cosmon_filestore::gate_slots::live_gate_slot_holders_in;

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

/// SIGKILL the holder, then its whole process group, so the orphaned command
/// does not outlive the test. Returns once the holder is reaped.
fn kill_group(holder: &mut Child) {
    let pgid = holder.id();
    holder.kill().unwrap();
    holder.wait().unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pgid}")])
        .status();
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
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

    let mut holder = spawn_slot(&state, "full_gate", &["sleep", "300"]);
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
    let pgid = holder.id();
    holder.kill().unwrap();
    holder.wait().unwrap();
    wait_until("the waiter to run", || marker.exists());
    assert!(waiter.wait().unwrap().success());
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{pgid}")])
        .status();
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

    let mut full = spawn_slot(&state, "full_gate", &["sleep", "300"]);
    wait_until("full_gate to be held", || {
        live_gate_slot_holders_in(&state, "full_gate").unwrap() == 1
    });
    let status = cs(&state)
        .args(["gates", "slot", "--pool", "docker_build", "--", "true"])
        .status()
        .unwrap();
    assert!(status.success(), "docker_build waited for full_gate");
    kill_group(&mut full);
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

    let mut holder = spawn_slot(&state, "full_gate", &["sleep", "300"]);
    wait_until("the slot to be held", || {
        live_gate_slot_holders_in(&state, "full_gate").unwrap() == 1
    });
    run_runner(&state, &out, &["touch", ran.to_str().unwrap()]);
    assert!(out.join("gates.queued").exists());
    std::thread::sleep(Duration::from_millis(600));
    assert!(!out.join("gates.pid").exists() && !out.join("gates.exit").exists());
    assert!(!ran.exists());

    kill_group(&mut holder);
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
