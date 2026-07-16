// SPDX-License-Identifier: AGPL-3.0-only

//! Operator-presence sensor — exogenous to the cosmon state-store.
//!
//! # The no-cloning theorem
//!
//! Cosmon already maintains an **endogenous** presence registry under
//! `.cosmon/state/presence/<sid>.json` — see [`crate::presence`]. That
//! file is a chalk-mark left by a session about itself: heartbeat
//! timestamp, headline, current molecule. It is fine for fleet
//! introspection.
//!
//! This module is a different thing: it asks the OS — not cosmon —
//! whether a human operator is *physically present*. Why bother?
//! Because the moment an operator-event-stream reads "present ⇔ this
//! session emitted a heartbeat in the last N seconds", the system is
//! observing **the performance of presence**, not presence itself: the
//! agent has full causal access to the signal it claims to measure.
//! That is the no-cloning theorem applied to the operator/agent
//! boundary — a presence sensor whose readout is downstream of cosmon's
//! own writes is structurally a tautology.
//!
//! Fix: the sensor reads from a substrate cosmon does **not** write —
//! `ioreg` HID idle counters on Darwin, `who`/`utmp` on POSIX, an
//! external phone or calendar feed in the future.
//!
//! # The discipline rule
//!
//! **Any [`PresenceSensor`] implementation is forbidden from reading
//! `.cosmon/state/`** (or anywhere downstream of cosmon's own writes,
//! more generally). Concrete sensors live in an adapter crate
//! (`cosmon_transport::presence_sensor`); the **coupling lint** that
//! ships beside them (`cosmon_transport::presence_sensor::coupling_lint`)
//! enforces this rule with a source-text scan of every shipped impl —
//! an offending line trips an explicit, named test failure rather than
//! passing silently. The seal is a *trace, not a lock* (same model as
//! briefing seals, [ADR-047]) — a motivated adversary with filesystem
//! access can rewrite both the source and the lint, but the lazy
//! shadow contract is caught.
//!
//! [ADR-047]: ../../docs/adr/047-event-log-protocol-v0.md
//!
//! # Naming
//!
//! [`OperatorPresence`] is the readout type, intentionally distinct
//! from [`crate::presence::Presence`] which models a session's own
//! chalk-mark. The two never alias: a session-presence file is
//! `present iff heartbeat fresh`; an operator-presence readout is
//! `present iff the human moved a mouse / hit a key / showed up on
//! some independent channel`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Where a [`PresenceSensor`] readout came from. Recorded inside the
/// readout so consumers can decide whether to trust it (e.g. an HID
/// idle counter is fine for "is the operator at the keyboard?", but
/// useless for "is the operator awake?" — that wants a watch or
/// phone signal).
///
/// Also embedded inside [`crate::event_v2::EventV2::OperatorPresent`] /
/// [`crate::event_v2::EventV2::OperatorAbsent`] envelopes so a downstream
/// consumer can apply the no-cloning theorem at decision time:
/// destructive-action gating MUST require `source != Internal`. The
/// [`Self::Internal`] variant exists for completeness — cosmon-lab
/// heartbeats and self-reported chalk-marks land here so the field is
/// never silently absent — but a sensor-grade consumer treats it as
/// "do not trust" rather than as a positive presence signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceSource {
    /// Darwin `ioreg -c IOHIDSystem` — kernel-maintained HID idle
    /// counter. Resets on keyboard, mouse, trackpad, stylus activity.
    Ioreg,
    /// POSIX `who` / `utmp` — terminal login state. Coarse but
    /// available cross-platform.
    Who,
    /// Cosmon's own heartbeat / `presence/<sid>.json` chalk-mark.
    /// Tautological with respect to the agent — present iff the
    /// agent emitted a heartbeat — and therefore the no-cloning
    /// theorem forbids destructive-action consumers from acting on
    /// this source.
    Internal,
    /// A future remote sensor (phone proximity, calendar busy block,
    /// physical badge, …). The string is a free-form discriminator
    /// the consumer can match on; a richer typed enum is deliberately
    /// deferred until at least one concrete impl exists.
    External(String),
}

impl PresenceSource {
    /// Is this source exogenous to cosmon — i.e. cosmon does not
    /// write the substrate that produced the readout?
    ///
    /// Returns `false` only for [`Self::Internal`]. Destructive-action
    /// consumers (`cs done`, `git push`, deletes) MUST require
    /// `is_exogenous() == true` to honour the no-cloning theorem.
    /// `External` is treated as exogenous; the discriminator string
    /// is informative only.
    #[must_use]
    pub const fn is_exogenous(&self) -> bool {
        !matches!(self, Self::Internal)
    }
}

/// Result of polling a [`PresenceSensor`].
///
/// `Unknown` is a first-class state — it is **not** an error. A
/// sensor that cannot probe right now (the binary is missing, the
/// kernel surface returned an unexpected payload, the network sensor
/// is offline) returns `Unknown` rather than fabricating a `Present`
/// or `Absent`. Consumers MUST treat `Unknown` as "do not act on
/// presence assumptions in this tick" — it is the cautious-default
/// state that protects destructive operations from silent failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorPresence {
    /// Operator activity observed within the sensor's freshness
    /// window. `last_activity` is a best-effort timestamp of the most
    /// recent event the sensor saw.
    Present {
        /// Best-effort timestamp of the most recent operator event.
        last_activity: SystemTime,
        /// Which substrate produced this readout.
        source: PresenceSource,
    },
    /// No operator activity within the sensor's freshness window.
    /// `since` is a best-effort timestamp of when the silence began.
    Absent {
        /// Best-effort timestamp of when the silence began.
        since: SystemTime,
        /// Which substrate produced this readout.
        source: PresenceSource,
    },
    /// The sensor could not produce a verdict this tick. Distinct
    /// from [`PresenceError`]: `Unknown` is a normal, frequently
    /// transient outcome (sensor not available on this platform,
    /// idle counter reset boundary, etc.). Errors are reserved for
    /// programming bugs and unrecoverable I/O.
    Unknown,
}

/// Failure modes for [`PresenceSensor::poll`]. See
/// [`OperatorPresence::Unknown`] for the normal "cannot probe right
/// now" path — these variants are reserved for cases where the
/// caller has done something wrong or the kernel surface has
/// genuinely failed.
#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    /// Underlying I/O failure (binary missing, permission denied,
    /// process spawn failed). The message is the OS-level
    /// description.
    #[error("presence sensor I/O failure: {0}")]
    Io(String),
    /// The sensor's substrate produced output we could not parse.
    /// Carries the offending payload so a future contributor can
    /// reproduce the parse failure deterministically.
    #[error("presence sensor parse failure: {0}")]
    Parse(String),
    /// The sensor is not supported on this platform / target. Used
    /// only at construction time — `poll()` never returns
    /// `Unsupported`; an unsupported sensor either fails to compile
    /// or returns [`OperatorPresence::Unknown`].
    #[error("presence sensor unsupported on this platform: {0}")]
    Unsupported(String),
}

/// Read operator-presence from a substrate cosmon does not write.
///
/// Implementations MUST satisfy the no-cloning rule documented at the
/// top of this module: they MAY NOT read from `.cosmon/state/`, the
/// fleet runtime registry, or any path whose content cosmon writes.
/// The coupling lint shipped beside the adapters
/// (`cosmon_transport::presence_sensor::coupling_lint`) enforces this
/// with a static source-text scan; the rule is structural, not a test
/// convenience.
///
/// `poll()` is synchronous and intended to be cheap enough to call on
/// every tick of a presence-aware loop (a few milliseconds at most).
/// If a sensor needs network or long-running probes, wrap it in a
/// caching adapter rather than blocking the caller.
///
/// # Examples
///
/// ```no_run
/// use cosmon_core::presence_sensor::{OperatorPresence, PresenceSensor};
///
/// fn act_only_when_operator_present(sensor: &dyn PresenceSensor) {
///     match sensor.poll() {
///         Ok(OperatorPresence::Present { .. }) => {
///             // safe to perform an attention-stealing action
///         }
///         Ok(OperatorPresence::Absent { .. }) | Ok(OperatorPresence::Unknown) => {
///             // defer until presence is positively confirmed
///         }
///         Err(_) => {
///             // log and treat as Unknown
///         }
///     }
/// }
/// ```
pub trait PresenceSensor: Send + Sync {
    /// Probe the underlying substrate and return a current readout.
    ///
    /// MUST NOT read from `.cosmon/state/` or any cosmon-written
    /// path. MUST return [`OperatorPresence::Unknown`] when the
    /// sensor is unavailable rather than fabricating a verdict.
    ///
    /// # Errors
    ///
    /// Returns [`PresenceError::Io`] if the underlying substrate
    /// (binary, kernel surface, network feed) failed in a way the
    /// caller may want to log; [`PresenceError::Parse`] if the
    /// substrate's payload could not be decoded;
    /// [`PresenceError::Unsupported`] only at construction-adjacent
    /// paths. A sensor that simply has nothing to say this tick
    /// returns `Ok(OperatorPresence::Unknown)` rather than an error.
    fn poll(&self) -> Result<OperatorPresence, PresenceError>;
}

/// A no-op sensor that always returns [`OperatorPresence::Unknown`].
///
/// Useful as a default when a target platform has no concrete impl
/// and the caller wants explicit "do not act on presence" semantics
/// rather than a compile-time error. It reads no files at all, so it
/// trivially satisfies the no-cloning rule (the coupling lint in the
/// adapter crate scans only the concrete, process-spawning sensors).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnknownSensor;

impl PresenceSensor for UnknownSensor {
    fn poll(&self) -> Result<OperatorPresence, PresenceError> {
        Ok(OperatorPresence::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_sensor_returns_unknown() {
        let s = UnknownSensor;
        assert_eq!(s.poll().unwrap(), OperatorPresence::Unknown);
    }

    #[test]
    fn presence_source_external_carries_label() {
        let src = PresenceSource::External("phone-proximity-v1".to_owned());
        match src {
            PresenceSource::External(label) => assert_eq!(label, "phone-proximity-v1"),
            _ => panic!("expected External"),
        }
    }

    #[test]
    fn operator_presence_variants_are_distinct() {
        let now = SystemTime::now();
        let p = OperatorPresence::Present {
            last_activity: now,
            source: PresenceSource::Ioreg,
        };
        let a = OperatorPresence::Absent {
            since: now,
            source: PresenceSource::Ioreg,
        };
        assert_ne!(p, a);
        assert_ne!(p, OperatorPresence::Unknown);
        assert_ne!(a, OperatorPresence::Unknown);
    }

    #[test]
    fn presence_sensor_is_object_safe() {
        // Compile-time check: `dyn PresenceSensor` exists.
        let _: Box<dyn PresenceSensor> = Box::new(UnknownSensor);
    }

    #[test]
    fn presence_error_displays_human_readable() {
        let err = PresenceError::Io("ioreg not found".to_owned());
        assert!(err.to_string().contains("ioreg not found"));
        let err = PresenceError::Parse("HIDIdleTime missing".to_owned());
        assert!(err.to_string().contains("HIDIdleTime missing"));
    }
}
