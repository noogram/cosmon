// SPDX-License-Identifier: AGPL-3.0-only

//! Machine-admission policy — the typed host observation and the I/O-free
//! decision over it that lets a dispatcher refuse *before* it spawns a worker
//! onto a machine that is already out of headroom (noogram/cosmon #58,
//! stage 1).
//!
//! # What this module is, and what it deliberately is not
//!
//! Two halves of the problem are separable, and only one of them belongs in
//! the domain core:
//!
//! - **Reading the host** is I/O, platform-specific, and untestable without
//!   the platform. It is a *port* here — [`MachineProbe`] — with no
//!   implementation, exactly as [`crate::transport::TransportBackend`]
//!   declares a spawn without performing one. The platform adapter is issue
//!   #58's Child B and lives outside this crate.
//! - **Deciding from a reading** is a total function over values, so it is
//!   here in full, unit-testable with struct literals and no privilege, no
//!   filesystem and no host: [`decide_admission`].
//!
//! The shape is copied deliberately from
//! [`crate::root_spawn_policy::decide_root_spawn`], for the reason stated
//! there: the real dispatch site cannot be exercised in a unit test, so the
//! *decision* is factored out and the effect half is left to the caller.
//! Nothing in this module is wired to a call site. `cs tackle` is untouched
//! by stage 1; the admission check reaches it in stage 3.
//!
//! # The one outcome a parse failure must never produce
//!
//! Every counter on [`MachineSnapshot`] is an [`Option`] and **an unread
//! counter is `None`, never `Some(0)`**. The asymmetry is not stylistic.
//! `used_swap_bytes: Some(0)` is the reading of a *perfectly healthy*
//! machine, so a probe that defaults an unparseable counter to zero reports
//! maximal headroom at exactly the moment it has stopped being able to see.
//! The failure is silent, it is indistinguishable from good news, and it
//! survives every test written against a working parser. `None` is
//! distinguishable, and [`decide_admission`] handles it explicitly.
//!
//! # Fail closed on the number, open on the verdict
//!
//! The two directions are opposite on purpose. On the *number*, unknown is
//! unknown: nothing is invented. On the *verdict*, an unknown counter
//! **admits**. A dispatcher whose only reading is "I could not read" must
//! keep dispatching, because the alternative is that an OS releasing a new
//! `sysctl` format, or a container hiding a `/proc` file, turns a whole fleet
//! into a dispatch outage that looks like a deliberate policy. A resource
//! gate that can fail into a stopwork switch is worse than no gate.
//!
//! # Why there is no `PressureLevel` enum here
//!
//! An earlier shape for this module classified the host into a
//! `#[non_exhaustive]` enum of named states (`Normal` / `Warn` / `Critical`).
//! It is not here, and the semver price is why. `#[non_exhaustive]` forces a
//! catch-all arm at every external `match` site *permanently*: the compiler
//! stops being able to tell a caller that a new state exists and is
//! unhandled, which on a gate whose entire job is to refuse means a new
//! pressure state silently falls into whichever arm the catch-all points at
//! — and the catch-all on an admission gate points at "admit". Dropping
//! `#[non_exhaustive]` instead prices every future state as a major bump of a
//! crate the whole workspace depends on. Neither price buys anything here,
//! because the classification is not ours to make: the kernel already
//! publishes its own level, and it is carried verbatim as one more raw
//! counter ([`MachineSnapshot::kernel_memory_pressure_level`]).
//!
//! [`AdmissionDecision`] itself is therefore *exhaustive* — two variants,
//! admit or refuse, and a caller that stops handling one stops compiling.
//! [`ResourceRefusal`], the reason attached to a refusal, is
//! `#[non_exhaustive]`, because a new reason genuinely is additive: a caller
//! that does not recognise it still knows the dispatch was refused, which is
//! the load-bearing half.
//!
//! # When this stops being ADR-free
//!
//! Stage 1 creates no new authority, no persisted artifact, no writer role
//! and no cross-galaxy object; it adds one refusal reason to a verb that
//! already has four, none of which needed an ADR. The trigger for the next
//! author is precise: **the moment admission acquires a machine-wide
//! artifact in the user-level cosmon root** — a cross-fleet lock, a
//! host-scoped worker roster, any file one galaxy writes and another reads —
//! it has created a shared writer and ADR-052's one-writer discipline
//! applies. Write the ADR then, not before.

use chrono::{DateTime, Utc};

/// Why a host reading could not be taken.
///
/// `#[non_exhaustive]` for the same reason [`crate::transport::TransportError`]
/// is: the port admits new structural failure modes as platform adapters land,
/// and none of them should cost the workspace a major bump. Note that this is
/// the error of *probing*, not of admission — a probe that fails does not
/// refuse a dispatch, it produces no reading, and [`decide_admission`] is
/// never consulted.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The platform has no implementation of this reading (or the adapter was
    /// built for a different one). Carries what was attempted.
    #[error("machine probe unsupported on this platform: {0}")]
    Unsupported(String),
    /// The reading failed at the OS boundary — a `sysctl` refused, a `/proc`
    /// file was absent or unreadable, a subprocess did not run.
    #[error("machine probe I/O error: {0}")]
    Io(String),
}

/// One instantaneous reading of the host's resource counters.
///
/// # Every field is an `Option`, and `None` is load-bearing
///
/// A counter this snapshot could not read is [`None`]. It is **never**
/// defaulted to zero. `used_swap_bytes: Some(0)` and
/// `available_memory_bytes: Some(0)` are both *legitimate readings of real
/// machines* — the first of a healthy one, the second of a dying one — so a
/// zero substituted for a parse failure is not merely imprecise, it is a
/// confident statement about the host that happens to be the opposite of what
/// was observed. An implementor of [`MachineProbe`] that cannot parse a
/// counter must leave it `None` and let [`decide_admission`] apply the
/// admit-on-unknown rule stated in the module doc.
///
/// `#[non_exhaustive]`: new counters (page-in rate, cgroup limits, per-fleet
/// accounting) are additive and must not cost a major bump. Construct with
/// [`MachineSnapshot::unread_at`] and assign the fields a probe actually read.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MachineSnapshot {
    /// When the reading was taken. Carried because a snapshot is evidence:
    /// a refusal that names a measurement without naming when it was measured
    /// cannot be checked against anything an operator sees afterwards
    /// (ADR-052 I8 — `MeasurementEmission`).
    pub sampled_at: DateTime<Utc>,
    /// Total physical memory, in bytes.
    pub total_memory_bytes: Option<u64>,
    /// Physical memory the kernel reports as available for allocation without
    /// reclaim, in bytes. Deliberately *available*, not *free*: free memory on
    /// a warm host is near zero by design and says nothing about headroom.
    pub available_memory_bytes: Option<u64>,
    /// Total configured swap, in bytes. `Some(0)` is a real reading — a host
    /// with swap disabled — and is not an error.
    pub total_swap_bytes: Option<u64>,
    /// Swap currently in use, in bytes.
    pub used_swap_bytes: Option<u64>,
    /// The kernel's **own** memory-pressure level, carried verbatim as the
    /// integer the platform publishes (on macOS, `kern.memorystatus_vm_pressure_level`).
    ///
    /// Uninterpreted on purpose: the scale differs per platform, and cosmon
    /// does not own it. See the module doc for why this is a raw counter
    /// rather than an enum of cosmon's own invention. No decision in this
    /// module reads it yet; it is carried so a refusal can quote it and so
    /// stage 2 can calibrate against it without a new probe.
    pub kernel_memory_pressure_level: Option<u32>,
    /// Logical CPU count as the OS reports it.
    pub logical_cpu_count: Option<u32>,
    /// Load averages over 1, 5 and 15 minutes, in that order. Carried as read;
    /// no decision in this module divides them by anything.
    pub load_average: Option<[f64; 3]>,
}

impl MachineSnapshot {
    /// A snapshot in which **nothing has been read yet**, stamped `sampled_at`.
    ///
    /// This is the only constructor, and it exists so that the honest state is
    /// also the cheapest one to write: a probe starts from all-unknown and
    /// fills in what it managed to read, so forgetting a counter leaves it
    /// `None` rather than zero. The struct is `#[non_exhaustive]`, so callers
    /// outside this crate cannot build one by literal and must come through
    /// here.
    #[must_use]
    pub const fn unread_at(sampled_at: DateTime<Utc>) -> Self {
        Self {
            sampled_at,
            total_memory_bytes: None,
            available_memory_bytes: None,
            total_swap_bytes: None,
            used_swap_bytes: None,
            kernel_memory_pressure_level: None,
            logical_cpu_count: None,
            load_average: None,
        }
    }

    /// Swap usage as a whole percentage of configured swap, when both halves
    /// were read and swap is configured at all.
    ///
    /// [`None`] covers three genuinely different situations that share one
    /// property — there is no ratio to compare against a ceiling: either
    /// counter unread, or a host with swap disabled (`total == 0`), where the
    /// ratio is undefined rather than zero and dividing would trap.
    ///
    /// Integer arithmetic throughout, in `u128`, so a byte count near `u64::MAX`
    /// cannot overflow the `× 100` and wrap a saturated host into a healthy
    /// reading.
    #[must_use]
    pub fn swap_used_percent(&self) -> Option<u8> {
        let (used, total) = (self.used_swap_bytes?, self.total_swap_bytes?);
        if total == 0 {
            return None;
        }
        let percent = u128::from(used) * 100 / u128::from(total);
        // A host reporting used > total is nonsense, but clamping is still
        // better than a panicking cast: 100 keeps it on the refusing side.
        Some(u8::try_from(percent).unwrap_or(100))
    }
}

/// The ceiling an admission decision is taken against.
///
/// # Why the threshold is an `Option` that defaults to `None`
///
/// `None` means *no ceiling*, and the whole mechanism is inert: every host
/// admits, byte-identically to a build without this module. That is
/// deliberate for stage 1, which ships the mechanism and calibrates nothing —
/// a number invented here would be a guess baked into a gate that refuses
/// work.
///
/// The precedent is already in the tree, and it is the closer of the two
/// `Option` thresholds there: `ModelBudgetConfig::strong_dispatch_cap`
/// (`crates/cosmon-core/src/config.rs:245`, reached from the `model_budget`
/// field at `:135`) is an `Option<u32>` documented as "`None` (the default)
/// disables the ceiling entirely". `HarvestAuthorityConfig::max_unintegrated`
/// (`crates/cosmon-core/src/config.rs:628`) is the same *shape* but the
/// opposite *semantics* — its `None` resolves to a default ceiling, because
/// there "no ceiling" is not expressible. This policy follows the former: an
/// uncalibrated admission gate must be absent, not defaulted.
///
/// Deliberately **not** `Copy`, though it is currently one `Option<u8>` wide.
/// [`decide_admission`] takes it by reference because it is a *configuration
/// value* that stage 2 grows — a memory ceiling, a load ceiling, a kernel
/// pressure ceiling — and a signature that flips from by-value to by-reference
/// the first time it crosses eight bytes is a churn every caller pays for. The
/// by-reference form is right for the type this will be, so it is the form it
/// has now.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdmissionPolicy {
    /// Refuse a dispatch when swap usage is **strictly above** this whole
    /// percentage of configured swap. `None` (the default) admits everything.
    ///
    /// Strictly above, so `Some(0)` still admits a host with no swap in use
    /// and refuses one with any, rather than being an unusable "refuse
    /// always". `Some(100)` admits everything a `u8` percentage can express,
    /// which is the intended reading of a ceiling nobody can exceed.
    pub max_swap_used_percent: Option<u8>,
}

impl AdmissionPolicy {
    /// A policy that admits every host, whatever it reads.
    ///
    /// Named rather than left to [`Default::default`] alone so a call site can
    /// say *why* it admits — the gate is uncalibrated — instead of merely
    /// defaulting.
    #[must_use]
    pub const fn inert() -> Self {
        Self {
            max_swap_used_percent: None,
        }
    }
}

/// Why a dispatch was refused on resource grounds.
///
/// Each variant carries **what was measured and what the ceiling was**, never
/// a bare verdict. An operator reading a refusal must be able to check it
/// against the host without re-deriving the policy, which is ADR-052 I8
/// (`MeasurementEmission`) applied at the value level rather than at the ledger.
///
/// `#[non_exhaustive]`: a new refusal reason is additive, and a caller that
/// does not recognise one still knows the dispatch was refused. Contrast
/// [`AdmissionDecision`], which is exhaustive precisely so that a caller
/// cannot stop handling a refusal without the compiler saying so.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRefusal {
    /// The host is swapping harder than the policy permits.
    SwapPressure {
        /// Swap in use at the moment of the reading, in bytes.
        used_swap_bytes: u64,
        /// Total configured swap at the moment of the reading, in bytes.
        total_swap_bytes: u64,
        /// The reading, as the whole percentage the ceiling is expressed in.
        observed_percent: u8,
        /// The ceiling that was exceeded — the policy's own number, copied
        /// into the refusal so the refusal is self-contained.
        ceiling_percent: u8,
    },
}

impl std::fmt::Display for ResourceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SwapPressure {
                used_swap_bytes,
                total_swap_bytes,
                observed_percent,
                ceiling_percent,
            } => write!(
                f,
                "host is swapping: {used_swap_bytes} of {total_swap_bytes} bytes of swap in use \
                 ({observed_percent}%), above the {ceiling_percent}% admission ceiling"
            ),
        }
    }
}

/// The decision the admission policy reaches for one host reading.
///
/// **Exhaustive on purpose** (no `#[non_exhaustive]`): this is the one match
/// whose arms must never silently acquire a default. A caller that gains a
/// third outcome should fail to compile until it decides what to do with it.
/// See the module doc for the semver price that buys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Dispatch may proceed. This is the outcome for a host with headroom,
    /// for an uncalibrated policy, **and** for a reading whose counters could
    /// not be taken — see the module doc's fail-open-on-the-verdict rule.
    Admit,
    /// Dispatch is refused on resource grounds, with the measurement and the
    /// ceiling that produced the verdict.
    Refuse {
        /// What was measured, and what it was measured against.
        refusal: ResourceRefusal,
    },
}

/// Decide whether a dispatch may proceed on the host `snapshot` describes.
///
/// A **pure total function**: no I/O, no clock, no panic, and a defined result
/// for every pair of inputs including snapshots in which nothing was read. It
/// is the whole of stage 1's judgement, and the effect half — emitting an exit
/// code, recording the refusal — belongs to the caller, exactly as
/// [`crate::root_spawn_policy::decide_root_spawn`] leaves the spawn to the
/// spawn site.
///
/// The rules, in the order they apply:
///
/// 1. No ceiling in the policy → [`AdmissionDecision::Admit`]. The mechanism
///    ships inert and stage 2 calibrates it.
/// 2. The counters the ceiling is about could not be read, or the host has no
///    swap configured → [`AdmissionDecision::Admit`]. Unknown is not
///    refusable; a probe that goes blind must not become a dispatch outage.
/// 3. Swap usage strictly above the ceiling → [`AdmissionDecision::Refuse`],
///    carrying the reading and the ceiling. At the ceiling exactly, admit.
#[must_use]
pub fn decide_admission(snapshot: &MachineSnapshot, policy: &AdmissionPolicy) -> AdmissionDecision {
    let Some(ceiling_percent) = policy.max_swap_used_percent else {
        return AdmissionDecision::Admit;
    };
    let Some(observed_percent) = snapshot.swap_used_percent() else {
        return AdmissionDecision::Admit;
    };
    if observed_percent <= ceiling_percent {
        return AdmissionDecision::Admit;
    }
    // `swap_used_percent` returned `Some`, so both counters were read and
    // total is non-zero; the fallbacks below are unreachable in practice and
    // are written as saturating defaults rather than as `expect`.
    AdmissionDecision::Refuse {
        refusal: ResourceRefusal::SwapPressure {
            used_swap_bytes: snapshot.used_swap_bytes.unwrap_or(0),
            total_swap_bytes: snapshot.total_swap_bytes.unwrap_or(0),
            observed_percent,
            ceiling_percent,
        },
    }
}

/// Hexagonal port for taking one host reading.
///
/// Object-safe (`&dyn MachineProbe` is a valid type) so a dispatcher can hold
/// a boxed probe chosen at run time — the real platform adapter in production,
/// a canned snapshot in a test — without this crate learning what a platform
/// is. It declares a reading and performs none, exactly as
/// [`crate::transport::TransportBackend`] declares a spawn and performs none:
/// the domain core stays I/O-free and the adapter lands outside it (issue #58,
/// Child B).
pub trait MachineProbe {
    /// Take one instantaneous reading of the host.
    ///
    /// An implementation that cannot read an individual counter must leave it
    /// [`None`] on the returned [`MachineSnapshot`] and still return `Ok` —
    /// a partial reading is a reading. `Err` is for a probe that produced no
    /// reading at all.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError::Unsupported`] when the platform has no
    /// implementation, and [`ProbeError::Io`] when the reading failed at the
    /// OS boundary.
    fn snapshot(&self) -> Result<MachineSnapshot, ProbeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time proof of the object-safety this port promises. A method
    /// that took `self` by value or introduced a generic parameter would break
    /// `&dyn MachineProbe` and fail here rather than at some future call site.
    fn _assert_object_safe(_probe: &dyn MachineProbe) {}

    /// A fixed instant, so every snapshot literal below reads as data rather
    /// than as a call to the clock this module does not have.
    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_764_000_000, 0).unwrap_or_default()
    }

    /// A host swapping over the ceiling is refused. The gate does something.
    #[test]
    fn refuses_under_critical_machine_pressure() {
        let mut snapshot = MachineSnapshot::unread_at(at());
        snapshot.total_swap_bytes = Some(1_000);
        snapshot.used_swap_bytes = Some(950);
        let policy = AdmissionPolicy {
            max_swap_used_percent: Some(80),
        };

        assert!(matches!(
            decide_admission(&snapshot, &policy),
            AdmissionDecision::Refuse { .. }
        ));
    }

    /// The false-positive guard. Without it, a gate that refused unconditionally
    /// would pass every other falsifier here — and a gate that always refuses is
    /// a stopwork switch wearing a policy's clothes.
    #[test]
    fn admits_on_a_host_with_headroom() {
        let mut snapshot = MachineSnapshot::unread_at(at());
        snapshot.total_memory_bytes = Some(64 * 1024 * 1024 * 1024);
        snapshot.available_memory_bytes = Some(48 * 1024 * 1024 * 1024);
        snapshot.total_swap_bytes = Some(1_000);
        snapshot.used_swap_bytes = Some(20);
        let policy = AdmissionPolicy {
            max_swap_used_percent: Some(80),
        };

        assert_eq!(
            decide_admission(&snapshot, &policy),
            AdmissionDecision::Admit
        );
    }

    /// Fail closed on the number, open on the verdict: a probe that read
    /// nothing admits. An OS format change must never become a fleet-wide
    /// dispatch outage that looks like deliberate policy.
    #[test]
    fn admits_when_counters_are_unknown() {
        let unread = MachineSnapshot::unread_at(at());
        let policy = AdmissionPolicy {
            max_swap_used_percent: Some(0),
        };

        assert_eq!(decide_admission(&unread, &policy), AdmissionDecision::Admit);

        // The same rule at the counter level: half a reading is still unknown,
        // and a host with swap disabled has no ratio rather than a ratio of 0.
        let mut half_read = MachineSnapshot::unread_at(at());
        half_read.used_swap_bytes = Some(u64::MAX);
        assert_eq!(
            decide_admission(&half_read, &policy),
            AdmissionDecision::Admit
        );

        let mut swapless = MachineSnapshot::unread_at(at());
        swapless.total_swap_bytes = Some(0);
        swapless.used_swap_bytes = Some(0);
        assert_eq!(
            decide_admission(&swapless, &policy),
            AdmissionDecision::Admit
        );
    }

    /// The refusal names the reading and the ceiling — ADR-052 I8
    /// (`MeasurementEmission`) made checkable at the value level. An operator
    /// reads numbers, not a verdict.
    #[test]
    fn refusal_carries_the_measured_values() {
        let mut snapshot = MachineSnapshot::unread_at(at());
        snapshot.total_swap_bytes = Some(8_000);
        snapshot.used_swap_bytes = Some(7_600);
        let policy = AdmissionPolicy {
            max_swap_used_percent: Some(90),
        };

        let AdmissionDecision::Refuse { refusal } = decide_admission(&snapshot, &policy) else {
            panic!("a host at 95% swap must be refused under a 90% ceiling");
        };
        assert_eq!(
            refusal,
            ResourceRefusal::SwapPressure {
                used_swap_bytes: 7_600,
                total_swap_bytes: 8_000,
                observed_percent: 95,
                ceiling_percent: 90,
            }
        );
        // And the rendered form quotes them, so the numbers survive the trip
        // to a terminal.
        let shown = refusal.to_string();
        for fragment in ["7600", "8000", "95%", "90%"] {
            assert!(
                shown.contains(fragment),
                "refusal must name {fragment}: {shown}"
            );
        }
    }

    /// The threshold is absent by default, so the mechanism ships inert: a
    /// build that merges stage 1 refuses nothing it refused before.
    #[test]
    fn a_default_policy_admits_everything() {
        assert_eq!(AdmissionPolicy::default(), AdmissionPolicy::inert());
        assert_eq!(AdmissionPolicy::default().max_swap_used_percent, None);

        let mut saturated = MachineSnapshot::unread_at(at());
        saturated.total_swap_bytes = Some(1_000);
        saturated.used_swap_bytes = Some(1_000);
        saturated.kernel_memory_pressure_level = Some(4);
        assert_eq!(
            decide_admission(&saturated, &AdmissionPolicy::default()),
            AdmissionDecision::Admit
        );
    }

    /// At the ceiling exactly, admit. The boundary is stated in the doc and is
    /// the difference between `Some(0)` meaning "any swap at all refuses" and
    /// `Some(0)` meaning "refuse always".
    #[test]
    fn the_ceiling_itself_admits() {
        let mut at_ceiling = MachineSnapshot::unread_at(at());
        at_ceiling.total_swap_bytes = Some(100);
        at_ceiling.used_swap_bytes = Some(80);
        let policy = AdmissionPolicy {
            max_swap_used_percent: Some(80),
        };
        assert_eq!(
            decide_admission(&at_ceiling, &policy),
            AdmissionDecision::Admit
        );
    }

    /// A byte count near `u64::MAX` must not wrap the `× 100` and turn a
    /// saturated host into a healthy reading.
    #[test]
    fn a_huge_swap_reading_does_not_overflow_the_ratio() {
        let mut huge = MachineSnapshot::unread_at(at());
        huge.total_swap_bytes = Some(u64::MAX);
        huge.used_swap_bytes = Some(u64::MAX);
        assert_eq!(huge.swap_used_percent(), Some(100));
    }
}
