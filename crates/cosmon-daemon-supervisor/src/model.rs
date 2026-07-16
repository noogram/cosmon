// SPDX-License-Identifier: AGPL-3.0-only

//! Typestate [`Child`] state machine.
//!
//! A supervised child process cycles through five states:
//!
//! ```text
//!                 ┌───────────────┐
//!        spawn ──▶│   Spawning    │── spawned ──▶ Running
//!        ▲        └───────────────┘                  │
//!        │                                    exited │
//!        │                                           ▼
//!    Respawning ◀── elapsed ── Throttling ◀── exit ─ Exited
//!        │                                           │
//!        └───────────── direct (throttle=0) ─────────┘
//! ```
//!
//! The typestate pattern (same trick `cosmon-core::rig` uses) enforces these
//! transitions at the *type* level: attempting to `spawn()` a `Child<Running>`
//! is a compile error, not a runtime panic. Each transition is a consuming
//! method that returns the next state with the relevant bookkeeping attached.
//!
//! Serializable form is the plain [`ChildStatus`] enum, which is what ends up
//! in `daemon-supervisor.state.json`; the typestate is a *compile-time* aid,
//! not a wire format.

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ChildStatus — serializable form
// ---------------------------------------------------------------------------

/// Wire-format status of a supervised child. One variant per typestate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    /// Between `fork()` and a received pid / SIGCHLD confirmation. Transient.
    Spawning,
    /// Child is alive — the supervisor knows its pid.
    Running,
    /// Child terminated. Either destined for [`ChildStatus::Throttling`]
    /// (throttle > 0) or directly to [`ChildStatus::Respawning`].
    Exited,
    /// Waiting out the configured `throttle_seconds` before respawn.
    Throttling,
    /// Throttle window elapsed, about to call the process port again.
    Respawning,
}

// ---------------------------------------------------------------------------
// Typestate markers
// ---------------------------------------------------------------------------

mod sealed {
    /// Prevents external crates from implementing `ChildState`.
    pub trait Sealed {}
}

/// Trait bound for [`Child`] state markers. Sealed — only the five states
/// defined in this module may implement it. (The same seal pattern
/// `cosmon-core::rig` uses.)
pub trait ChildState: sealed::Sealed {
    /// The corresponding serializable status.
    fn status() -> ChildStatus;
}

/// Typestate marker: the child is mid-spawn.
#[derive(Debug, Clone, Copy)]
pub struct Spawning {
    _private: PhantomData<()>,
}

/// Typestate marker: the child is alive.
#[derive(Debug, Clone, Copy)]
pub struct Running {
    /// OS pid assigned by the process port.
    pub pid: u32,
    /// When the supervisor observed the child alive.
    pub since: DateTime<Utc>,
}

/// Typestate marker: the child terminated.
#[derive(Debug, Clone, Copy)]
pub struct Exited {
    /// Unix exit code if the port could collect one; `None` on signal kills
    /// that don't give us a code (rare in practice on macOS/Linux).
    pub exit_code: Option<i32>,
    /// Wall-clock time of death, used as the throttle anchor.
    pub at: DateTime<Utc>,
}

/// Typestate marker: the child is parked until `until`.
#[derive(Debug, Clone, Copy)]
pub struct Throttling {
    /// Earliest wall-clock time the supervisor may respawn.
    pub until: DateTime<Utc>,
}

/// Typestate marker: the throttle window has elapsed, respawn pending.
#[derive(Debug, Clone, Copy)]
pub struct Respawning {
    /// Observed "now" at which the throttle elapsed.
    pub at: DateTime<Utc>,
}

impl sealed::Sealed for Spawning {}
impl sealed::Sealed for Running {}
impl sealed::Sealed for Exited {}
impl sealed::Sealed for Throttling {}
impl sealed::Sealed for Respawning {}

impl ChildState for Spawning {
    fn status() -> ChildStatus {
        ChildStatus::Spawning
    }
}
impl ChildState for Running {
    fn status() -> ChildStatus {
        ChildStatus::Running
    }
}
impl ChildState for Exited {
    fn status() -> ChildStatus {
        ChildStatus::Exited
    }
}
impl ChildState for Throttling {
    fn status() -> ChildStatus {
        ChildStatus::Throttling
    }
}
impl ChildState for Respawning {
    fn status() -> ChildStatus {
        ChildStatus::Respawning
    }
}

// ---------------------------------------------------------------------------
// Child<S>
// ---------------------------------------------------------------------------

/// Typestate-parameterised record of one supervised child.
///
/// The `state: S` marker is what the compiler tracks; the other fields are
/// common across every state so they can be inspected from any `impl<S>`
/// block. The **name** of the child (i.e. the [`crate::DaemonSpec::name`])
/// is cheap to clone and we keep it as the join key with the spec map.
#[derive(Debug, Clone)]
pub struct Child<S: ChildState> {
    name: String,
    /// Number of respawns observed since the supervisor process started
    /// managing this child. Lets the operator spot crash loops.
    respawn_count: u32,
    state: S,
}

// --- Shared accessors ------------------------------------------------------

impl<S: ChildState> Child<S> {
    /// The daemon's name, as declared in `daemons.toml`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Lifetime respawn count for this supervised child.
    #[must_use]
    pub const fn respawn_count(&self) -> u32 {
        self.respawn_count
    }

    /// Serializable status corresponding to the current typestate.
    #[must_use]
    pub fn status() -> ChildStatus {
        S::status()
    }

    /// The state marker (read-only).
    #[must_use]
    pub const fn state(&self) -> &S {
        &self.state
    }
}

// --- Spawning --------------------------------------------------------------

impl Child<Spawning> {
    /// Construct a brand-new child in the `Spawning` state.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            respawn_count: 0,
            state: Spawning {
                _private: PhantomData,
            },
        }
    }

    /// The process port confirmed a pid; transition to `Running`.
    #[must_use]
    pub fn spawned(self, pid: u32, since: DateTime<Utc>) -> Child<Running> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count,
            state: Running { pid, since },
        }
    }
}

// --- Running ---------------------------------------------------------------

impl Child<Running> {
    /// The supervisor observed the child die; transition to `Exited`.
    #[must_use]
    pub fn exited(self, exit_code: Option<i32>, at: DateTime<Utc>) -> Child<Exited> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count,
            state: Exited { exit_code, at },
        }
    }

    /// OS pid of the running child.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.state.pid
    }

    /// When the supervisor last observed the child alive.
    #[must_use]
    pub const fn since(&self) -> DateTime<Utc> {
        self.state.since
    }
}

// --- Exited ----------------------------------------------------------------

impl Child<Exited> {
    /// Park this child until `until` has elapsed.
    ///
    /// Use this when `throttle_seconds > 0`. For `throttle_seconds == 0` call
    /// [`Self::respawn_immediately`] instead — the compiler then enforces that
    /// no zero-duration `Throttling` instance can exist.
    #[must_use]
    pub fn throttle(self, until: DateTime<Utc>) -> Child<Throttling> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count,
            state: Throttling { until },
        }
    }

    /// Skip the throttle window entirely (used when the spec has
    /// `throttle_seconds = 0` — Niel's cheapest path).
    #[must_use]
    pub fn respawn_immediately(self, at: DateTime<Utc>) -> Child<Respawning> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count,
            state: Respawning { at },
        }
    }

    /// Exit code if the port captured one.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.state.exit_code
    }

    /// Observed time of death.
    #[must_use]
    pub const fn at(&self) -> DateTime<Utc> {
        self.state.at
    }
}

// --- Throttling ------------------------------------------------------------

impl Child<Throttling> {
    /// Observed "now" has caught up with `until`; become `Respawning`.
    #[must_use]
    pub fn elapsed(self, at: DateTime<Utc>) -> Child<Respawning> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count,
            state: Respawning { at },
        }
    }

    /// Earliest wall-clock the supervisor may call the process port again.
    #[must_use]
    pub const fn until(&self) -> DateTime<Utc> {
        self.state.until
    }
}

// --- Respawning ------------------------------------------------------------

impl Child<Respawning> {
    /// The supervisor has (re-)issued the spawn call; back to `Spawning`
    /// with the respawn counter bumped.
    #[must_use]
    pub fn spawn(self) -> Child<Spawning> {
        Child {
            name: self.name,
            respawn_count: self.respawn_count.saturating_add(1),
            state: Spawning {
                _private: PhantomData,
            },
        }
    }

    /// Observed "now" when the throttle elapsed.
    #[must_use]
    pub const fn at(&self) -> DateTime<Utc> {
        self.state.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(n: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(n, 0).unwrap()
    }

    #[test]
    fn happy_path_cycles_through_every_state() {
        let c = Child::<Spawning>::new("tg-bot");
        assert_eq!(c.name(), "tg-bot");
        assert_eq!(c.respawn_count(), 0);
        assert_eq!(Child::<Spawning>::status(), ChildStatus::Spawning);

        let c = c.spawned(42, t(10));
        assert_eq!(c.pid(), 42);
        assert_eq!(Child::<Running>::status(), ChildStatus::Running);

        let c = c.exited(Some(1), t(30));
        assert_eq!(c.exit_code(), Some(1));
        assert_eq!(c.at(), t(30));
        assert_eq!(Child::<Exited>::status(), ChildStatus::Exited);

        let c = c.throttle(t(60));
        assert_eq!(c.until(), t(60));
        assert_eq!(Child::<Throttling>::status(), ChildStatus::Throttling);

        let c = c.elapsed(t(61));
        assert_eq!(c.at(), t(61));
        assert_eq!(Child::<Respawning>::status(), ChildStatus::Respawning);

        let c = c.spawn();
        assert_eq!(c.respawn_count(), 1);
    }

    #[test]
    fn respawn_immediately_skips_throttle() {
        let c = Child::<Spawning>::new("x")
            .spawned(1, t(0))
            .exited(None, t(1))
            .respawn_immediately(t(1))
            .spawn();
        assert_eq!(c.respawn_count(), 1);
    }

    #[test]
    fn respawn_count_saturates_not_overflows() {
        // Manually construct a Respawning with u32::MAX — if we ever log a
        // supervisor on for a geological age, `spawn()` must not panic.
        let c = Child::<Respawning> {
            name: "x".into(),
            respawn_count: u32::MAX,
            state: Respawning { at: t(0) },
        };
        let c = c.spawn();
        assert_eq!(c.respawn_count(), u32::MAX);
    }

    #[test]
    fn child_status_roundtrips_through_json() {
        for s in [
            ChildStatus::Spawning,
            ChildStatus::Running,
            ChildStatus::Exited,
            ChildStatus::Throttling,
            ChildStatus::Respawning,
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: ChildStatus = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
        }
    }

    // The next two lines would fail to compile if uncommented — a compile-
    // time proof that invalid transitions are impossible. Kept as a doc
    // comment so rustdoc shows intent without gating the test build.
    //
    // let _ = Child::<Running>::new("x");      // no `new` on Running
    // let c = Child::<Running>::new("x");
    // let _ = c.spawned(1, t(0));               // no `spawned` on Running
}
