// SPDX-License-Identifier: AGPL-3.0-only

//! `cs gates slot` — run a command while holding a gate slot (ADR-190 D1,
//! noogram/cosmon#214).
//!
//! The project config names the pools and their sizes (`[gate_slots]`); this
//! verb waits for a free slot of one pool, supervises a command group with the
//! caller's stdio, and exits with the command's code when it ran to completion.
//! `scripts/run-gates-bg.sh` goes through it for the full gate
//! bundle, and a project's own heavy command can do the same under a pool name
//! of its choosing.
//!
//! The slot is a kernel `flock` held by this process for the life of the
//! command. By default the lock descriptor is close-on-exec, so a `kill -9`
//! of the wrapper frees the slot even when the command survives. A named,
//! bounded pool can opt into handing the same descriptor to the command.
//!
//! # Lock order
//!
//! `slot → trunk → fleet`. This verb takes a slot and nothing else; it never
//! takes the trunk lock, and `cs done` never calls it, so a landing is not
//! queued behind a gate.

use std::fs::{self, File};
use std::future::pending;
use std::io::{self, Seek as _, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use chrono::Utc;
use cosmon_core::config::FULL_GATE_POOL;
use cosmon_filestore::gate_slots::{
    acquire_gate_slot_in, register_gate_waiter, try_acquire_gate_slot_in, GateRunRecord,
    GateSlotGuard,
};
use nix::errno::Errno;
use nix::sys::signal::{killpg, pthread_sigmask, SigSet, SigmaskHow, Signal};
use nix::unistd::{getpgrp, tcgetpgrp, tcsetpgrp, Pid};
use tokio::signal::unix::{signal, SignalKind};

use super::Context;

const SIGNAL_GRACE: Duration = Duration::from_millis(250);
const OUTPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Arguments for `cs gates`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    sub: Sub,
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Run a command while holding a slot of a pool, waiting for one to be free
    Slot(SlotArgs),
}

#[derive(clap::Args)]
struct SlotArgs {
    /// Pool to take a slot from (sized by `[gate_slots]` in `.cosmon/config.toml`)
    #[arg(long, default_value = FULL_GATE_POOL, value_name = "NAME")]
    pool: String,

    /// Print the configured slot count of the pool and exit 0, or exit 3 when
    /// the pool is unbounded; runs no command
    #[arg(long)]
    configured: bool,

    /// Create this file once the slot is held, just before the command starts
    #[arg(long, value_name = "PATH")]
    ready_file: Option<PathBuf>,

    /// Kill the command group after this many seconds of runtime, excluding slot wait
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..), conflicts_with = "configured")]
    timeout: Option<u64>,

    /// Require a writable directory before and after the command runs
    #[arg(long, value_name = "PATH", conflicts_with = "configured")]
    require_dir: Option<PathBuf>,

    /// Write a typed verdict (`ran:<code>`, `timed_out`, or `could_not_run:<reason>`)
    #[arg(long, value_name = "PATH", conflicts_with = "configured")]
    verdict_file: Option<PathBuf>,

    /// Let the command inherit a bounded named slot until its last descriptor closes
    #[arg(long, conflicts_with = "configured")]
    hand_descriptor: bool,

    /// The command to run, after `--`
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

/// Execute `cs gates`.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.sub {
        Sub::Slot(a) => slot(ctx, a),
    }
}

fn slot(ctx: &Context, a: &SlotArgs) -> Result<()> {
    let config = cosmon_filestore::load_project_config(&ctx.config_path()).unwrap_or_default();
    let count = config.gate_slots.count_for(&a.pool);

    if a.configured {
        return match count {
            Some(n) => {
                println!("{n}");
                Ok(())
            }
            None => std::process::exit(3),
        };
    }
    let Some((program, rest)) = a.command.split_first() else {
        bail!("no command given: usage `cs gates slot [--pool NAME] -- <command…>`");
    };
    if a.hand_descriptor && (a.pool == FULL_GATE_POOL || count.is_none()) {
        bail!("--hand-descriptor requires a bounded named pool other than `full_gate`");
    }
    if let Some(path) = &a.verdict_file {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot clear verdict file {}", path.display()))
            }
        }
    }

    let record = GateRunRecord {
        pid: std::process::id(),
        pgid: None,
        pool: a.pool.clone(),
        command: a.command.clone(),
        started_at: Utc::now(),
        last_output_at: None,
        output_observed: false,
    };
    let guard = count
        .map(|n| wait_for_slot(&ctx.state_dir(), &record, n))
        .transpose()?;
    let mut command = Command::new(program);
    command.args(rest).process_group(0);
    if let Some(guard) = guard.as_ref().filter(|_| a.hand_descriptor) {
        let fd = guard.as_fd().as_raw_fd();
        // SAFETY: the guard keeps fd open across spawn. fcntl is async-signal-safe,
        // and only the child's descriptor flags change before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let verdict = run_slot(
        a,
        program,
        command,
        guard.as_ref().map(|guard| (guard, record)),
    )?;
    if let Some(path) = &a.verdict_file {
        verdict.write(path)?;
    }
    let code = verdict.report();
    drop(guard);
    std::process::exit(code);
}

fn run_slot(
    a: &SlotArgs,
    program: &str,
    command: Command,
    observation: Option<(&GateSlotGuard, GateRunRecord)>,
) -> Result<Verdict> {
    let required_dir_error = || a.require_dir.as_deref().and_then(probe_directory);
    if let Some(reason) = required_dir_error() {
        return Ok(Verdict::CouldNotRun(reason));
    }
    if let Some(path) = &a.ready_file {
        fs::write(path, std::process::id().to_string())
            .with_context(|| format!("cannot write ready file {}", path.display()))?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start gate supervisor")?;
    let timeout = a.timeout.map(Duration::from_secs);
    let result = runtime.block_on(supervise(command, program, timeout, observation));
    if let Some(reason) = required_dir_error() {
        return Ok(Verdict::CouldNotRun(reason));
    }
    match result {
        Err(err) if a.verdict_file.is_some() => Ok(Verdict::CouldNotRun(format!("{err:#}"))),
        result => result,
    }
}

/// Probe with the same filesystem operations the gate needs for new output.
fn probe_directory(path: &Path) -> Option<String> {
    tempfile::Builder::new()
        .prefix(".cs-gate-probe-")
        .tempfile_in(path)
        .and_then(|mut probe| {
            probe.write_all(b"\n")?;
            probe.close()
        })
        .err()
        .map(|err| format!("required directory {}: {err}", path.display()))
}

enum Verdict {
    Ran(i32),
    TimedOut,
    CouldNotRun(String),
}

impl Verdict {
    fn write(&self, path: &Path) -> Result<()> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let mut file = tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))
            .with_context(|| format!("cannot create temporary verdict for {}", path.display()))?;
        match self {
            Self::Ran(code) => write!(file, "ran:{code}"),
            Self::TimedOut => write!(file, "timed_out"),
            Self::CouldNotRun(reason) => {
                write!(file, "could_not_run:{}", reason.replace(['\r', '\n'], " "))
            }
        }
        .with_context(|| format!("cannot write verdict file {}", path.display()))?;
        file.persist(path)
            .with_context(|| format!("cannot publish verdict file {}", path.display()))?;
        Ok(())
    }

    fn report(self) -> i32 {
        match self {
            Self::Ran(code) => code,
            Self::TimedOut => {
                eprintln!("cs gates slot: command timed out");
                124
            }
            Self::CouldNotRun(reason) => {
                eprintln!("cs gates slot: could not run: {reason}");
                125
            }
        }
    }
}

async fn supervise(
    mut command: Command,
    program: &str,
    timeout: Option<Duration>,
    mut observation: Option<(&GateSlotGuard, GateRunRecord)>,
) -> Result<Verdict> {
    // Subscribe before spawn so a signal received as the child starts is queued.
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut next_signal = async || {
        tokio::select! {
            _ = terminate.recv() => Signal::SIGTERM,
            _ = interrupt.recv() => Signal::SIGINT,
            _ = hangup.recv() => Signal::SIGHUP,
        }
    };
    let _terminal = ForegroundTerminal::lend_to(&mut command);
    let mut output = observation
        .as_ref()
        .and_then(|_| OutputOffsets::open().ok());
    let started_at = Utc::now();
    let mut child = tokio::process::Command::from(command)
        .spawn()
        .with_context(|| format!("cannot run `{program}`"))?;
    let id = child.id().context("gate command has no pid")?;
    let group = Pid::from_raw(i32::try_from(id)?);
    if let Some((guard, record)) = &mut observation {
        record.pgid = Some(id);
        record.started_at = started_at;
        record.output_observed = output.is_some();
        if let Err(err) = guard.write_record(record) {
            eprintln!(
                "cs gates slot: cannot record holder of {}: {err}",
                guard.path().display()
            );
            output = None;
        }
    }
    let deadline = async {
        match timeout {
            Some(duration) => tokio::time::sleep(duration).await,
            None => pending().await,
        }
    };
    tokio::pin!(deadline);
    let mut timed_out = tokio::select! {
        biased;
        () = &mut deadline => true,
        received = next_signal() => {
            signal_group(group, received)?;
            false
        }
        status = child.wait() => {
            sample_output(&mut output, &mut observation);
            return Ok(Verdict::Ran(exit_code(status?)));
        }
        never = observe_output(&mut output, &mut observation) => match never {},
    };
    if !timed_out {
        let grace = tokio::time::sleep(SIGNAL_GRACE);
        tokio::pin!(grace);
        timed_out = loop {
            tokio::select! {
                biased;
                () = &mut deadline => break true,
                () = &mut grace => break false,
                received = next_signal() => signal_group(group, received)?,
            }
        };
    }
    // Reap only after signalling the group. Even an exited leader pins its
    // PID until wait, and other members may still ignore the forwarded signal.
    signal_group(group, Signal::SIGKILL)?;
    let status = child.wait().await?;
    sample_output(&mut output, &mut observation);
    if timed_out {
        Ok(Verdict::TimedOut)
    } else {
        Ok(Verdict::Ran(exit_code(status)))
    }
}

/// Observe writes to the inherited open file descriptions. Borrowing their
/// offsets preserves the command's stdio, including a shared `2>&1` offset.
/// Pipes, terminals and devices do not expose a written-byte offset.
struct OutputOffsets {
    files: [File; 2],
    positions: [u64; 2],
}

impl OutputOffsets {
    fn open() -> io::Result<Self> {
        let mut files = [
            File::from(io::stdout().as_fd().try_clone_to_owned()?),
            File::from(io::stderr().as_fd().try_clone_to_owned()?),
        ];
        for file in &files {
            if !file.metadata()?.is_file() {
                return Err(io::ErrorKind::Unsupported.into());
            }
        }
        let positions = [files[0].stream_position()?, files[1].stream_position()?];
        Ok(Self { files, positions })
    }

    fn advanced(&mut self) -> io::Result<bool> {
        let positions = [
            self.files[0].stream_position()?,
            self.files[1].stream_position()?,
        ];
        let advanced = positions
            .iter()
            .zip(self.positions)
            .any(|(now, before)| *now > before);
        self.positions = positions;
        Ok(advanced)
    }
}

async fn observe_output(
    output: &mut Option<OutputOffsets>,
    observation: &mut Option<(&GateSlotGuard, GateRunRecord)>,
) -> std::convert::Infallible {
    if output.is_some() {
        let mut interval = tokio::time::interval(OUTPUT_SAMPLE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while output.is_some() {
            interval.tick().await;
            sample_output(output, observation);
        }
    }
    pending().await
}

fn sample_output(
    output: &mut Option<OutputOffsets>,
    observation: &mut Option<(&GateSlotGuard, GateRunRecord)>,
) {
    let (Some(offsets), Some((guard, record))) = (output.as_mut(), observation) else {
        return;
    };
    match offsets.advanced() {
        Ok(false) => return,
        Ok(true) => record.last_output_at = Some(Utc::now()),
        Err(_) => {
            record.output_observed = false;
            *output = None;
        }
    }
    if let Err(err) = guard.write_record(record) {
        eprintln!(
            "cs gates slot: cannot update holder of {}: {err}",
            guard.path().display()
        );
        *output = None;
    }
}

fn signal_group(group: Pid, signal: Signal) -> Result<()> {
    match killpg(group, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot send {signal} to gate group {group}")),
    }
}

/// Preserve direct terminal reads for a caller that owns the foreground.
struct ForegroundTerminal {
    file: File,
    original_group: Pid,
}

impl ForegroundTerminal {
    fn lend_to(command: &mut Command) -> Option<Self> {
        let file = File::open("/dev/tty").ok()?;
        let original_group = getpgrp();
        if tcgetpgrp(&file) != Ok(original_group) {
            return None;
        }
        let fd = file.as_raw_fd();
        // SAFETY: the returned guard keeps fd open across spawn. The child
        // is already in its own process group here. Only signal-mask and
        // terminal syscalls run in the hook.
        unsafe {
            command.pre_exec(move || {
                set_foreground(BorrowedFd::borrow_raw(fd), getpgrp()).map_err(io::Error::from)
            });
        }
        Some(Self {
            file,
            original_group,
        })
    }
}

impl Drop for ForegroundTerminal {
    fn drop(&mut self) {
        let _ = set_foreground(self.file.as_fd(), self.original_group);
    }
}

fn set_foreground(terminal: BorrowedFd<'_>, group: Pid) -> nix::Result<()> {
    // A process changing the foreground from the background must block SIGTTOU.
    let mut block = SigSet::empty();
    block.add(Signal::SIGTTOU);
    let mut original = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&block), Some(&mut original))?;
    let result = tcsetpgrp(terminal, group);
    pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&original), None)?;
    result
}

/// Take a slot, saying once on stderr that this run is queued.
fn wait_for_slot(state_dir: &Path, record: &GateRunRecord, count: usize) -> Result<GateSlotGuard> {
    let pool = &record.pool;
    if let Some(guard) = try_acquire_gate_slot_in(state_dir, pool, count)? {
        return Ok(guard);
    }
    let queued = register_gate_waiter(state_dir, record).map_err(|err| {
        eprintln!("cs gates slot: cannot record queued run: {err}");
    });
    eprintln!("cs gates slot: queued for a slot of pool `{pool}` ({count} slots, all held)");
    let guard = acquire_gate_slot_in(state_dir, pool, count)?;
    drop(queued);
    eprintln!(
        "cs gates slot: slot {} of pool `{pool}` acquired",
        guard.index()
    );
    Ok(guard)
}

/// The command's exit code; `128 + signal` for a command killed by a signal.
fn exit_code(status: ExitStatus) -> i32 {
    if let Some(sig) = status.signal() {
        return 128 + sig;
    }
    status.code().unwrap_or(1)
}
