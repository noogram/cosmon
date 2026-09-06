// SPDX-License-Identifier: AGPL-3.0-only

//! Production [`MachineProbe`] adapter — the platform reader behind the port
//! `cosmon-core` declares (noogram/cosmon #58, stage 1, Child B).
//!
//! The *port* is [`cosmon_core::admission::MachineProbe`], in the zero-I/O
//! domain crate. This is its production *adapter*, and it lives here for the
//! reason already recorded in `crates/cosmon-core/src/harness.rs:156-161`:
//! `RealCommandRunner` was moved out of the domain crate so `cosmon-core`
//! performs no process I/O (INV-DOMAIN-PURE-NO-IO, ADR-082). Reading a host
//! is process I/O, so the same boundary puts the reader here and leaves
//! `decide_admission` — a total function over values — there.
//!
//! # Why there is no `sysinfo`-class dependency
//!
//! Four `sysctl` reads and one `vm_stat` read, with their parsers, are under
//! two hundred lines and go through [`CommandRunner`], the process seam this
//! workspace already owns. That buys three things a crate would not: the
//! probe is mockable through the port that already exists, no entry appears
//! in the root `Cargo.toml` that every crate then sees, and `Cargo.lock` does
//! not move under an in-review stack. The cost is that a counter no command
//! prints cannot be read — and the rule for that case is to leave the counter
//! [`None`] and say so, never to add the dependency unilaterally.
//!
//! # How available memory is derived, and why that derivation is stated here
//!
//! darwin publishes no "available memory" counter. It is derived from
//! `vm_stat` as
//!
//! ```text
//! available = (pages free + pages inactive + pages speculative) × page size
//! ```
//!
//! where the page size is read from `vm_stat`'s own header line rather than
//! assumed — it is 4096 on `x86_64` darwin and 16384 on arm64, and hard-coding
//! either produces a four-fold error on the other.
//!
//! The three terms are the pages the kernel can hand to a new allocation
//! without evicting a running program's working set: free pages, speculative
//! read-ahead pages (dropped on demand), and the inactive queue (the reclaim
//! target). Deliberately **excluded**: *active* and *wired* pages, which are
//! in use; the compressor's occupied pages, which are already-compressed
//! memory rather than headroom; and *purgeable* pages, which are not a
//! separate queue at all but a subset of the active and inactive counts, so
//! adding them double-counts.
//!
//! The derivation is an approximation of a quantity darwin does not export,
//! and it is the field most likely to be wrong. It is written down here so a
//! future reader disputes a stated rule instead of reverse-engineering an
//! arithmetic expression.
//!
//! # Every parse failure is `None`, never `0`
//!
//! Restated from the port because this is the module that can violate it:
//! a `sysctl` that is absent, exits non-zero, or prints a format this parser
//! does not recognise leaves its counter [`None`] and the *rest of the
//! snapshot populated*. `Some(0)` is the reading of a healthy host for swap
//! and of a dying one for memory; substituting it for "I could not see" makes
//! a blind probe indistinguishable from good news.
//!
//! `Err` is reserved for a probe that produced no reading at all: a
//! non-darwin host ([`ProbeError::Unsupported`]) or a darwin host on which
//! every single read failed ([`ProbeError::Io`], naming them). Nothing here
//! panics, so a caller can always continue.

use std::path::PathBuf;

use chrono::Utc;
use cosmon_core::admission::{MachineProbe, MachineSnapshot, ProbeError};
use cosmon_core::harness::CommandRunner;

use crate::command_runner::RealCommandRunner;

/// Bytes in one mebibyte — the unit `vm.swapusage` prints its `M` suffix in.
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// `u64::MAX` as a float, written as a literal rather than cast so the bound
/// check below does not itself have to justify a lossy `u64 as f64`.
const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;

/// Reads the host's resource counters through the [`CommandRunner`] port.
///
/// Generic over the runner rather than holding a `Box<dyn CommandRunner>` so
/// the production path pays no virtual call and a test can inject
/// `cosmon_core::harness::MockCommandRunner` by value. The type still
/// satisfies the object-safe [`MachineProbe`] port, so a dispatcher can hold
/// `Box<dyn MachineProbe>` over it.
#[derive(Debug, Clone)]
pub struct HostMachineProbe<R: CommandRunner> {
    runner: R,
    /// Working directory the reads are spawned in.
    ///
    /// The commands ignore it, but [`CommandRunner::exec`] requires one and
    /// [`std::process::Command`] fails outright if it does not exist — so the
    /// probe carries a directory it knows is present rather than inheriting
    /// a caller's cwd, which on a worker may have been deleted under it.
    cwd: PathBuf,
}

impl HostMachineProbe<RealCommandRunner> {
    /// A probe that reads this machine through [`RealCommandRunner`].
    ///
    /// The constructor production code wants; [`HostMachineProbe::with_runner`]
    /// is the seam a test uses.
    #[must_use]
    pub fn real() -> Self {
        Self::with_runner(RealCommandRunner)
    }
}

impl<R: CommandRunner> HostMachineProbe<R> {
    /// A probe reading through the supplied `runner`.
    ///
    /// Exists so the whole reader — command names, argument order, parse
    /// failures, the all-reads-failed path — is exercisable without the host
    /// being in the state under test. A probe only ever checked against a
    /// healthy machine is a probe whose failure behaviour is unknown.
    #[must_use]
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            cwd: std::env::temp_dir(),
        }
    }

    /// Run `cmd args…` and return its stdout, or [`None`] if the process could
    /// not be spawned or exited non-zero.
    ///
    /// A non-zero exit is deliberately not distinguished from a spawn failure:
    /// both mean this counter was not read, and the port's contract for that
    /// is one value, [`None`].
    fn read(&self, cmd: &str, args: &[&str]) -> Option<String> {
        let out = self.runner.exec(cmd, args, self.cwd.as_path()).ok()?;
        if out.success() {
            Some(out.stdout)
        } else {
            None
        }
    }

    /// Read one `sysctl` key's raw value.
    fn sysctl(&self, key: &str) -> Option<String> {
        self.read("sysctl", &["-n", key])
    }
}

impl<R: CommandRunner> MachineProbe for HostMachineProbe<R> {
    fn snapshot(&self) -> Result<MachineSnapshot, ProbeError> {
        if !cfg!(target_os = "macos") {
            // Honest report, not a fabricated reading: the counters below are
            // darwin's, and nothing here infers anything else from the target
            // beyond which reader to use.
            return Err(ProbeError::Unsupported(
                "only darwin (sysctl + vm_stat) has a reader; this host has none".to_owned(),
            ));
        }

        let mut snapshot = MachineSnapshot::unread_at(Utc::now());
        // Names of the reads that produced nothing, in the order attempted.
        // A total failure has to say *what* failed, and a partial one is not
        // an error at all.
        let mut failed: Vec<&str> = Vec::new();

        match self.sysctl("vm.swapusage").as_deref().map(parse_swapusage) {
            Some((total, used)) if total.is_some() || used.is_some() => {
                snapshot.total_swap_bytes = total;
                snapshot.used_swap_bytes = used;
            }
            _ => failed.push("vm.swapusage"),
        }

        snapshot.total_memory_bytes = self.sysctl("hw.memsize").as_deref().and_then(parse_u64);
        if snapshot.total_memory_bytes.is_none() {
            failed.push("hw.memsize");
        }

        snapshot.logical_cpu_count = self.sysctl("hw.ncpu").as_deref().and_then(parse_u32);
        if snapshot.logical_cpu_count.is_none() {
            failed.push("hw.ncpu");
        }

        snapshot.load_average = self.sysctl("vm.loadavg").as_deref().and_then(parse_loadavg);
        if snapshot.load_average.is_none() {
            failed.push("vm.loadavg");
        }

        snapshot.kernel_memory_pressure_level = self
            .sysctl("kern.memorystatus_vm_pressure_level")
            .as_deref()
            .and_then(parse_u32);
        if snapshot.kernel_memory_pressure_level.is_none() {
            failed.push("kern.memorystatus_vm_pressure_level");
        }

        snapshot.available_memory_bytes = self
            .read("vm_stat", &[])
            .as_deref()
            .and_then(parse_available_memory_bytes);
        if snapshot.available_memory_bytes.is_none() {
            failed.push("vm_stat");
        }

        if failed.len() == READS_ATTEMPTED {
            return Err(ProbeError::Io(format!(
                "every host read failed: {}",
                failed.join(", ")
            )));
        }
        Ok(snapshot)
    }
}

/// How many independent reads [`HostMachineProbe::snapshot`] attempts. Only a
/// run in which *all* of them fail is a total failure; the constant is here so
/// that adding a read without extending this count cannot silently turn the
/// total-failure test into an unreachable branch.
const READS_ATTEMPTED: usize = 6;

/// Parse `vm.swapusage` into `(total, used)` bytes.
///
/// The format is `total = 3072.00M  used = 1536.50M  free = 1535.50M`. Each
/// half is parsed independently, so a line whose `used` field changes shape
/// still yields the `total`. A suffix this parser does not know is [`None`]
/// rather than a guess: mistaking mebibytes for bytes understates a saturated
/// host by six orders of magnitude and it reads as perfect health.
fn parse_swapusage(raw: &str) -> (Option<u64>, Option<u64>) {
    (swapusage_field(raw, "total"), swapusage_field(raw, "used"))
}

/// Pull one `name = <number><suffix>` field out of a `vm.swapusage` line.
fn swapusage_field(raw: &str, name: &str) -> Option<u64> {
    let after = raw.split(name).nth(1)?;
    let value = after.trim_start().strip_prefix('=')?.trim_start();
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let (digits, rest) = value.split_at(end);
    let scale = match rest.chars().next() {
        Some('K') => 1024.0,
        Some('M') => BYTES_PER_MIB,
        Some('G') => BYTES_PER_MIB * 1024.0,
        // A bare number is already bytes; anything else is a format this
        // parser has not been shown, and guessing its unit is the defect this
        // branch exists to prevent.
        Some(c) if c.is_whitespace() => 1.0,
        None => 1.0,
        Some(_) => return None,
    };
    let parsed: f64 = digits.parse().ok()?;
    if !parsed.is_finite() || parsed < 0.0 {
        return None;
    }
    let bytes = parsed * scale;
    // `as` on a float that exceeds `u64::MAX` saturates rather than wrapping
    // in Rust 2021, but the bound is checked anyway so the cast is provably
    // in range and the intent is legible.
    if bytes > U64_MAX_AS_F64 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(bytes.round() as u64)
}

/// Parse a whole `sysctl -n` scalar.
fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim().parse().ok()
}

/// Parse a whole `sysctl -n` scalar that must fit a `u32`.
fn parse_u32(raw: &str) -> Option<u32> {
    raw.trim().parse().ok()
}

/// Parse `vm.loadavg`, whose format is `{ 6.38 6.21 5.38 }`.
///
/// All three averages or none: a partial load vector has no meaning the
/// snapshot's `[f64; 3]` could carry.
fn parse_loadavg(raw: &str) -> Option<[f64; 3]> {
    let inner = raw.trim().trim_start_matches('{').trim_end_matches('}');
    let mut fields = inner.split_whitespace();
    let mut out = [0.0_f64; 3];
    for slot in &mut out {
        *slot = fields.next()?.parse().ok()?;
    }
    Some(out)
}

/// Derive available memory in bytes from `vm_stat` output.
///
/// See the module doc for the derivation and for what it deliberately leaves
/// out. Returns [`None`] unless the page size *and* all three page counts were
/// read: a sum missing a term is not a smaller number, it is a wrong one.
fn parse_available_memory_bytes(raw: &str) -> Option<u64> {
    let page_size = parse_vm_stat_page_size(raw)?;
    let free = vm_stat_count(raw, "Pages free")?;
    let inactive = vm_stat_count(raw, "Pages inactive")?;
    let speculative = vm_stat_count(raw, "Pages speculative")?;
    free.checked_add(inactive)?
        .checked_add(speculative)?
        .checked_mul(page_size)
}

/// Read the page size out of `vm_stat`'s header, e.g.
/// `Mach Virtual Memory Statistics: (page size of 16384 bytes)`.
///
/// Read, never assumed: 4096 on `x86_64` darwin and 16384 on arm64.
fn parse_vm_stat_page_size(raw: &str) -> Option<u64> {
    let after = raw.split("page size of").nth(1)?;
    let digits = after.trim_start();
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    parse_u64(digits.get(..end)?)
}

/// Read one `<label>: <count>.` row out of `vm_stat` output.
fn vm_stat_count(raw: &str, label: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let value = line.trim().strip_prefix(label)?.strip_prefix(':')?;
        parse_u64(value.trim().trim_end_matches('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::harness::{CommandOutput, MockCommandRunner};

    // The oracle. These are bytes captured from a real darwin host and stored
    // outside this file precisely so the expected values below can be computed
    // from them by hand rather than copied out of the parser: a parser
    // rewritten to agree with itself would still have to agree with these.
    const SWAPUSAGE_BUSY: &str =
        include_str!("../tests/fixtures/machine_probe/vm_swapusage_busy.txt");
    const SWAPUSAGE_IDLE: &str =
        include_str!("../tests/fixtures/machine_probe/vm_swapusage_idle.txt");
    const HW_MEMSIZE: &str = include_str!("../tests/fixtures/machine_probe/hw_memsize.txt");
    const HW_NCPU: &str = include_str!("../tests/fixtures/machine_probe/hw_ncpu.txt");
    const VM_LOADAVG: &str = include_str!("../tests/fixtures/machine_probe/vm_loadavg.txt");
    const PRESSURE: &str =
        include_str!("../tests/fixtures/machine_probe/kern_memorystatus_vm_pressure_level.txt");
    const VM_STAT: &str = include_str!("../tests/fixtures/machine_probe/vm_stat.txt");

    /// Script a mock runner with one healthy response per read, in the order
    /// [`HostMachineProbe::snapshot`] takes them.
    fn healthy_runner() -> MockCommandRunner {
        let runner = MockCommandRunner::new();
        for stdout in [
            SWAPUSAGE_BUSY,
            HW_MEMSIZE,
            HW_NCPU,
            VM_LOADAVG,
            PRESSURE,
            VM_STAT,
        ] {
            runner.script(CommandOutput::ok(stdout));
        }
        runner
    }

    /// Every field lands with the byte value the fixture implies. The
    /// expectations are arithmetic over the captured text, stated inline so a
    /// reader can check them without running anything.
    #[test]
    fn parses_a_known_sysctl_fixture() {
        let probe = HostMachineProbe::with_runner(healthy_runner());
        let snapshot = probe.snapshot().expect("a fully scripted host reads");

        // `total = 3072.00M` → 3072 × 1024 × 1024.
        assert_eq!(snapshot.total_swap_bytes, Some(3_221_225_472));
        // `used = 1536.50M` → 1536.5 × 1024 × 1024, the half-mebibyte included.
        assert_eq!(snapshot.used_swap_bytes, Some(1_611_137_024));
        assert_eq!(snapshot.swap_used_percent(), Some(50));

        assert_eq!(snapshot.total_memory_bytes, Some(137_438_953_472));
        assert_eq!(snapshot.logical_cpu_count, Some(16));
        assert_eq!(snapshot.load_average, Some([6.38, 6.21, 5.38]));
        assert_eq!(snapshot.kernel_memory_pressure_level, Some(1));

        // (2_118_213 free + 2_546_937 inactive + 148_025 speculative)
        //   × 16_384 bytes per page.
        assert_eq!(snapshot.available_memory_bytes, Some(78_859_059_200));

        // A host with swap disabled is a real reading of zero, not a failure,
        // and not a ratio.
        let (total, used) = parse_swapusage(SWAPUSAGE_IDLE);
        assert_eq!((total, used), (Some(0), Some(0)));
    }

    /// Byte/mebibyte confusion is the most likely real defect in this module,
    /// and swapping *available* for *total* is the second. Both are caught
    /// here by pinning the relations the values must satisfy — relations a
    /// mis-scaled or mis-assigned parser cannot satisfy by accident.
    #[test]
    fn a_unit_mistake_is_caught() {
        let probe = HostMachineProbe::with_runner(healthy_runner());
        let snapshot = probe.snapshot().expect("a fully scripted host reads");

        // The `M` suffix is mebibytes. A parser that dropped the scale would
        // report 3072 bytes here, and 3072 < 1 MiB.
        let total_swap = snapshot.total_swap_bytes.expect("swap total was scripted");
        assert!(
            total_swap > 1024 * 1024,
            "vm.swapusage `M` must scale to mebibytes, got {total_swap} bytes"
        );
        assert_eq!(total_swap, 3072 * 1024 * 1024);

        // Used is strictly below total in the fixture, so a parser that read
        // the same field twice, or swapped the two, fails.
        let used_swap = snapshot.used_swap_bytes.expect("swap used was scripted");
        assert!(
            used_swap < total_swap,
            "used ({used_swap}) must not be read as total ({total_swap})"
        );

        // Available is derived from three page queues and cannot equal total
        // physical memory; a probe that assigned `hw.memsize` to both fields
        // — the available-for-total mutation — fails here.
        let total_memory = snapshot
            .total_memory_bytes
            .expect("hw.memsize was scripted");
        let available = snapshot
            .available_memory_bytes
            .expect("vm_stat was scripted");
        assert_ne!(available, total_memory);
        assert!(
            available < total_memory,
            "available ({available}) cannot exceed physical ({total_memory})"
        );

        // And the page size is read, not assumed: the arm64 fixture's 16384
        // is four times the x86_64 default, so a hard-coded 4096 lands at a
        // quarter of the expected value.
        assert_eq!(parse_vm_stat_page_size(VM_STAT), Some(16_384));
    }

    /// One unreadable counter is `None`, the rest of the snapshot is
    /// populated, and nothing panics. The failing read is the *third* of six,
    /// so a probe that abandoned the run on first failure would also lose the
    /// three after it.
    #[test]
    fn an_unreadable_counter_is_none_not_zero() {
        let runner = MockCommandRunner::new();
        runner.script(CommandOutput::ok(SWAPUSAGE_BUSY));
        runner.script(CommandOutput::ok(HW_MEMSIZE));
        // `hw.ncpu` — a non-zero exit with nothing usable on stdout.
        runner.script(CommandOutput::err(1, "sysctl: unknown oid 'hw.ncpu'"));
        runner.script(CommandOutput::ok(VM_LOADAVG));
        // The pressure level exits 0 but prints something unparseable.
        runner.script(CommandOutput::ok("not-a-number\n"));
        runner.script(CommandOutput::ok(VM_STAT));

        let probe = HostMachineProbe::with_runner(runner);
        let snapshot = probe
            .snapshot()
            .expect("a partial reading is still a reading");

        assert_eq!(snapshot.logical_cpu_count, None, "unread is None, not 0");
        assert_eq!(
            snapshot.kernel_memory_pressure_level, None,
            "garbage on stdout is None, not 0"
        );
        // Everything after the failures still landed.
        assert_eq!(snapshot.load_average, Some([6.38, 6.21, 5.38]));
        assert_eq!(snapshot.available_memory_bytes, Some(78_859_059_200));
        assert_eq!(snapshot.total_memory_bytes, Some(137_438_953_472));
        assert_eq!(snapshot.total_swap_bytes, Some(3_221_225_472));
    }

    /// Every read fails: the caller gets a typed error naming what failed, not
    /// a default snapshot that reads as a healthy host.
    #[test]
    fn a_total_failure_is_a_typed_error() {
        let runner = MockCommandRunner::new();
        for _ in 0..READS_ATTEMPTED {
            runner.script(CommandOutput::err(1, "no such command"));
        }

        let probe = HostMachineProbe::with_runner(runner);
        let err = probe
            .snapshot()
            .expect_err("a host on which nothing reads produces no reading");

        let ProbeError::Io(reason) = &err else {
            panic!("a failed read is an I/O failure, not {err:?}");
        };
        for named in [
            "vm.swapusage",
            "hw.memsize",
            "hw.ncpu",
            "vm.loadavg",
            "kern.memorystatus_vm_pressure_level",
            "vm_stat",
        ] {
            assert!(reason.contains(named), "error must name {named}: {reason}");
        }
    }

    /// The command names and argument order are part of the contract with the
    /// host; a rename would otherwise only show up on a real machine.
    #[test]
    fn the_reads_are_the_documented_commands() {
        let probe = HostMachineProbe::with_runner(healthy_runner());
        let _ = probe.snapshot();
        let calls: Vec<String> = probe
            .runner
            .calls()
            .iter()
            // `RecordedCall`'s Display joins an empty argument list with a
            // trailing space; the command line is what is under test here.
            .map(|call| call.to_string().trim_end().to_owned())
            .collect();
        assert_eq!(
            calls,
            vec![
                "sysctl -n vm.swapusage".to_owned(),
                "sysctl -n hw.memsize".to_owned(),
                "sysctl -n hw.ncpu".to_owned(),
                "sysctl -n vm.loadavg".to_owned(),
                "sysctl -n kern.memorystatus_vm_pressure_level".to_owned(),
                "vm_stat".to_owned(),
            ]
        );
    }

    /// **The differential smoke.** Sample the probe and the native readers at
    /// the same moment on the real host and compare them.
    ///
    /// Two classes of check, deliberately different in strictness. The
    /// invariant counters — physical memory, logical CPUs, configured swap —
    /// must match the native reader *exactly*: they do not move between the
    /// two samples, so any difference is a parser defect. The volatile ones —
    /// available memory, load, kernel pressure — are sampled microseconds
    /// apart on a live machine and cannot be asserted equal; they are checked
    /// for *magnitude* (available memory within a factor of two of a native
    /// recomputation, and inside physical memory) and printed, because the
    /// point of this test is a number a human compares against the machine.
    ///
    /// Ignored unless the `integration` feature is on: it reads the host it
    /// runs on, so it is not a hermetic unit test and must not be part of the
    /// default gate. Run it with
    /// `cargo test -p cosmon-transport --features integration -- --nocapture
    /// differential`.
    #[test]
    #[cfg_attr(not(feature = "integration"), ignore = "reads the real host")]
    fn differential_against_the_native_readers() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let native = |cmd: &str, args: &[&str]| -> String {
            RealCommandRunner
                .exec(cmd, args, std::env::temp_dir().as_path())
                .map(|out| out.stdout)
                .unwrap_or_default()
        };

        let snapshot = HostMachineProbe::real()
            .snapshot()
            .expect("the real host reads");
        let raw_swap = native("sysctl", &["-n", "vm.swapusage"]);
        let raw_memsize = native("sysctl", &["-n", "hw.memsize"]);
        let raw_ncpu = native("sysctl", &["-n", "hw.ncpu"]);
        let raw_vm_stat = native("vm_stat", &[]);

        println!("probe:  {snapshot:#?}");
        println!("native vm.swapusage: {}", raw_swap.trim());
        println!("native hw.memsize:   {}", raw_memsize.trim());
        println!("native hw.ncpu:      {}", raw_ncpu.trim());

        // Invariant between the two samples — exact equality or a defect.
        assert_eq!(snapshot.total_memory_bytes, parse_u64(&raw_memsize));
        assert_eq!(snapshot.logical_cpu_count, parse_u32(&raw_ncpu));
        assert_eq!(snapshot.total_swap_bytes, parse_swapusage(&raw_swap).0);

        // Volatile — magnitude only.
        let native_available =
            parse_available_memory_bytes(&raw_vm_stat).expect("vm_stat parses on darwin");
        let probed = snapshot
            .available_memory_bytes
            .expect("available memory reads on darwin");
        println!("available: probe {probed} vs native {native_available}");
        let (lo, hi) = (probed.min(native_available), probed.max(native_available));
        assert!(
            hi <= lo.saturating_mul(2),
            "available memory diverged beyond a factor of two: {probed} vs {native_available}"
        );
        if let Some(total) = snapshot.total_memory_bytes {
            assert!(
                probed < total,
                "available ({probed}) exceeds physical ({total})"
            );
        }
    }

    /// A suffix the parser has not been shown is `None`, not a guess at its
    /// unit — the branch that stops a future `vm.swapusage` format from being
    /// silently misscaled.
    #[test]
    fn an_unknown_swap_suffix_is_refused() {
        assert_eq!(
            parse_swapusage("total = 12.00Z  used = 3.00Z"),
            (None, None)
        );
        // A bare byte count with no suffix is taken at face value.
        assert_eq!(
            parse_swapusage("total = 4096  used = 2048"),
            (Some(4096), Some(2048))
        );
    }
}
