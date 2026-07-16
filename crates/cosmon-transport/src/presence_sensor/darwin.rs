// SPDX-License-Identifier: AGPL-3.0-only

// Darwin presence sensor — reads HID idle counter from ioreg.
//
// THIS FILE IS UNDER THE COUPLING LINT (see coupling_lint.rs).
// It MUST NOT contain a path under cosmon's state directory. The
// coupling lint scans this file's source text and fails the build if
// the forbidden literal appears. The substrate read here is the kernel
// HID idle counter via the `ioreg` binary — exogenous to anything
// cosmon writes, which is exactly the process spawn the lint permits.

use std::process::Command;
use std::time::{Duration, SystemTime};

use cosmon_core::presence_sensor::{
    OperatorPresence, PresenceError, PresenceSensor, PresenceSource,
};

/// Darwin presence sensor backed by `ioreg -c IOHIDSystem`.
///
/// The kernel maintains a per-user HID idle counter that resets to
/// zero on every keyboard, mouse, trackpad, or stylus event. Reading
/// it is a few-millisecond fork+exec; safe to call once per tick.
///
/// # The no-cloning theorem in code form
///
/// This sensor opens **exactly one** external resource: the `ioreg`
/// binary in `PATH`. It does not read configuration files, JSON
/// state, event logs, or anything cosmon itself writes. The coupling
/// lint in `super::coupling_lint` proves this with a source-text scan
/// of this file.
///
/// # Threshold
///
/// The default idle threshold is 5 minutes. Past that, the sensor
/// reports [`OperatorPresence::Absent`]. Tune via
/// [`IoregSensor::with_threshold`] when a different policy is
/// appropriate (e.g. a destructive-operation gate may want a shorter
/// 30-second window).
pub struct IoregSensor {
    idle_threshold: Duration,
}

impl IoregSensor {
    /// Default sensor with a 5-minute idle threshold.
    #[must_use]
    pub fn new() -> Self {
        Self::with_threshold(Duration::from_secs(300))
    }

    /// Sensor with a caller-supplied idle threshold. A shorter
    /// threshold makes the sensor more conservative (more `Absent`
    /// readouts); a longer threshold tolerates more idle time.
    #[must_use]
    pub fn with_threshold(idle_threshold: Duration) -> Self {
        Self { idle_threshold }
    }

    /// Currently configured idle threshold.
    #[must_use]
    pub fn idle_threshold(&self) -> Duration {
        self.idle_threshold
    }
}

impl Default for IoregSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl PresenceSensor for IoregSensor {
    fn poll(&self) -> Result<OperatorPresence, PresenceError> {
        // ioreg unavailable — degrade to Unknown, do NOT fabricate a verdict.
        let Ok(output) = Command::new("ioreg").args(["-c", "IOHIDSystem"]).output() else {
            return Ok(OperatorPresence::Unknown);
        };

        if !output.status.success() {
            return Ok(OperatorPresence::Unknown);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(idle_ns) = parse_hid_idle_ns(&stdout) else {
            return Err(PresenceError::Parse(
                "HIDIdleTime not found in ioreg output".to_owned(),
            ));
        };

        let idle = Duration::from_nanos(idle_ns);
        let now = SystemTime::now();
        let activity_at = now.checked_sub(idle).unwrap_or(now);

        if idle < self.idle_threshold {
            Ok(OperatorPresence::Present {
                last_activity: activity_at,
                source: PresenceSource::Ioreg,
            })
        } else {
            Ok(OperatorPresence::Absent {
                since: activity_at,
                source: PresenceSource::Ioreg,
            })
        }
    }
}

/// Parse the first `HIDIdleTime = <nanoseconds>` line from
/// `ioreg -c IOHIDSystem` output.
///
/// Returns `None` if no such line is present (which on a healthy
/// macOS host is itself a signal that something is wrong with the
/// HID subsystem). Public-in-crate so the coupling lint can feed
/// recorded fixtures without touching the live system.
pub(crate) fn parse_hid_idle_ns(output: &str) -> Option<u64> {
    for line in output.lines() {
        if !line.contains("HIDIdleTime") {
            continue;
        }
        let eq_pos = line.find('=')?;
        let raw = line[eq_pos + 1..].trim();
        // ioreg sometimes prints the value bare, sometimes with a
        // trailing comment or whitespace; take the leading digits.
        let digits: String = raw.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        return digits.parse::<u64>().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_FRESH: &str = r#"+-o IOHIDSystem  <class IOHIDSystem, id 0x1000003fc, registered, matched, active, busy 0 (0 ms), retain 27>
    {
      "HIDIdleTime" = 1234567890
      "HIDPointerAcceleration" = 25600
    }
"#;

    const FIXTURE_LONG_IDLE: &str = r#"+-o IOHIDSystem
    {
      "HIDIdleTime" = 600000000000
    }
"#;

    const FIXTURE_NO_IDLE: &str = r#"+-o IOHIDSystem
    {
      "HIDPointerAcceleration" = 25600
    }
"#;

    #[test]
    fn parse_hid_idle_ns_extracts_value() {
        assert_eq!(parse_hid_idle_ns(FIXTURE_FRESH), Some(1_234_567_890));
    }

    #[test]
    fn parse_hid_idle_ns_handles_long_idle() {
        assert_eq!(parse_hid_idle_ns(FIXTURE_LONG_IDLE), Some(600_000_000_000));
    }

    #[test]
    fn parse_hid_idle_ns_returns_none_when_absent() {
        assert_eq!(parse_hid_idle_ns(FIXTURE_NO_IDLE), None);
    }

    #[test]
    fn parse_hid_idle_ns_handles_extra_whitespace() {
        let weird = r#"      "HIDIdleTime"    =    42  "#;
        assert_eq!(parse_hid_idle_ns(weird), Some(42));
    }

    #[test]
    fn ioreg_sensor_default_threshold_is_five_minutes() {
        let s = IoregSensor::new();
        assert_eq!(s.idle_threshold(), Duration::from_secs(300));
    }

    #[test]
    fn ioreg_sensor_with_threshold_overrides() {
        let s = IoregSensor::with_threshold(Duration::from_secs(30));
        assert_eq!(s.idle_threshold(), Duration::from_secs(30));
    }

    #[test]
    fn ioreg_sensor_default_trait() {
        let s: IoregSensor = IoregSensor::default();
        assert_eq!(s.idle_threshold(), Duration::from_secs(300));
    }

    #[test]
    fn poll_returns_a_value() {
        // Smoke test: on a Darwin host with ioreg available, poll()
        // returns Ok(...) — Present, Absent, or Unknown. We do not
        // assert which, since the test runner's idle state is
        // ambient.
        let s = IoregSensor::new();
        let r = s.poll().expect("poll should not error on macOS host");
        match r {
            OperatorPresence::Present { source, .. } | OperatorPresence::Absent { source, .. } => {
                assert_eq!(source, PresenceSource::Ioreg);
            }
            OperatorPresence::Unknown => {}
        }
    }
}
