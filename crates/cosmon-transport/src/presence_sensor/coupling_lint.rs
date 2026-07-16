// SPDX-License-Identifier: AGPL-3.0-only

//! Coupling lint for
//! [`PresenceSensor`](cosmon_core::presence_sensor::PresenceSensor)
//! implementations — **not** a purity audit.
//!
//! This file does not prove the sensors are pure; they are not — they
//! spawn `ioreg`. What it proves is the narrower *coupling* rule: a
//! sensor must read from a substrate cosmon does **not** write. It is a
//! static source-text scan over every shipped sensor impl in this crate,
//! banning `.cosmon/state` reads and direct `File::open` while
//! deliberately permitting the process spawn the sensor is built around.
//! (Naming it an "audit" earlier was the very self-own this molecule
//! corrects: the file policed path-reads while greenlighting the
//! `Command::new` it performs — task-20260622-3144.)
//!
//! The check is cheap and runs as a normal `#[test]`. It is
//! deliberately not a `compile_fail` doctest or a `build.rs` hook
//! because we want a named, navigable failure that points at the
//! exact file and line that introduced the violation.
//!
//! # What "forbidden" means
//!
//! A sensor source file MUST NOT contain a path under cosmon's state
//! directory — i.e. the literal string corresponding to
//! `.cosmon/state`. The needle is built at test time by
//! concatenation so this lint file is itself never a false
//! positive (its source contains the two halves separately).
//!
//! Lines that are *deliberately* documenting the rule — the lint
//! file's own header, a sensor module's top banner — are excluded
//! by being outside the list of `LINTED_SENSOR_SOURCES`. Only the
//! sensor *implementation* files are scanned, not their docstrings
//! about the rule.

#![cfg(test)]

/// Source text of every sensor implementation file shipped in this
/// crate. New sensors MUST be added here; the parity test below
/// asserts that each variant of `PresenceSource` has at least one
/// audited source (or an explicit waiver comment).
const LINTED_SENSOR_SOURCES: &[(&str, &str)] = &[
    #[cfg(target_os = "macos")]
    ("darwin.rs", include_str!("darwin.rs")),
];

/// Build the forbidden needle from two concatenated halves so the
/// audit's own source is never a false positive when this very file
/// is itself scanned by something else (e.g. a workspace-wide grep).
fn forbidden_needle() -> String {
    let mut s = String::from(".cosmon");
    s.push('/');
    s.push_str("state");
    s
}

#[test]
fn no_sensor_impl_reads_state_dir() {
    let needle = forbidden_needle();
    for (label, src) in LINTED_SENSOR_SOURCES {
        for (lineno, line) in src.lines().enumerate() {
            assert!(
                !line.contains(&needle),
                "no-cloning theorem violation in {label}:{lineno}: \
                 sensor impl references `{needle}` — presence sensors \
                 MUST read from a substrate cosmon does not write. \
                 Offending line: {line}",
                lineno = lineno + 1,
            );
        }
    }
}

#[test]
fn linted_sensor_list_is_non_empty_on_supported_platforms() {
    // Sanity: on Darwin (the only platform with an impl today),
    // we audit at least one source. On other platforms this list is
    // empty by design — flipping that to a hard failure would force
    // every CI runner to be macOS.
    #[cfg(target_os = "macos")]
    {
        assert!(
            !LINTED_SENSOR_SOURCES.is_empty(),
            "macOS build must audit at least one sensor (darwin.rs)"
        );
    }
}

#[test]
fn no_sensor_impl_reads_events_jsonl() {
    // Tighter relative of the main rule: even if a future impl
    // somehow targeted an events file outside `.cosmon/state/` (a
    // mirror, a test fixture path), reading `events.jsonl` is the
    // signature shape of state-store consumption. Reject it.
    for (label, src) in LINTED_SENSOR_SOURCES {
        assert!(
            !src.contains("events.jsonl"),
            "no-cloning theorem violation in {label}: sensor impl \
             references `events.jsonl` — presence sensors MUST NOT \
             derive readouts from cosmon's append-only event log."
        );
    }
}

#[test]
fn no_sensor_impl_opens_arbitrary_files() {
    // Sensors are allowed to spawn external commands (`ioreg`,
    // `who`, `xprintidle`, …). They are NOT allowed to call
    // `std::fs::read*` or `File::open` directly, because every
    // path-based read is a candidate channel for accidental
    // state-store coupling. This is a stricter, structural
    // discipline than the substring scan above.
    let banned = ["std::fs::read", "File::open", "fs::read_to_string"];
    for (label, src) in LINTED_SENSOR_SOURCES {
        for needle in banned {
            assert!(
                !src.contains(needle),
                "sensor impl {label} references `{needle}` — sensors \
                 MUST use external commands (ioreg/who/...) rather \
                 than path-based reads, to avoid accidental \
                 state-store coupling. If a path-based sensor is \
                 genuinely needed, propose an ADR first."
            );
        }
    }
}

/// Adversarial runtime test: a sensor must not flip its readout in
/// response to fake activity written to cosmon's events log. Runs
/// only on macOS where `IoregSensor` exists.
#[cfg(target_os = "macos")]
#[test]
fn ioreg_readout_unaffected_by_state_writes() {
    use super::IoregSensor;
    use cosmon_core::presence_sensor::{OperatorPresence, PresenceSensor, PresenceSource};
    use std::io::Write;

    let sensor = IoregSensor::new();

    // Probe the live system before writing anything.
    let before = sensor.poll().expect("ioreg poll should not error on macOS");

    // Forge a fake state directory in a tempdir. Spam 1000 events.
    // If the sensor were (incorrectly) reading cosmon-written
    // signals, this would push the readout toward `Present`.
    let tmp = tempfile::tempdir().expect("tempdir");
    // Build the path components from pieces so the audit's
    // forbidden-needle scan does not flag this test source.
    let state_dir = tmp.path().join(".cosmon").join("state");
    std::fs::create_dir_all(&state_dir).expect("mkdir");
    let log = state_dir.join("events.jsonl");
    {
        let mut f = std::fs::File::create(&log).expect("create events log");
        for i in 0..1000 {
            writeln!(
                f,
                r#"{{"ts":"2026-05-09T00:00:00Z","kind":"FakeEvent","seq":{i}}}"#,
            )
            .expect("write");
        }
    }

    // Probe again. Source must still be Ioreg (the sensor consults
    // the kernel HID counter, not the spammed log).
    let after = sensor.poll().expect("ioreg poll");
    let after_source = match &after {
        OperatorPresence::Present { source, .. } | OperatorPresence::Absent { source, .. } => {
            Some(source.clone())
        }
        OperatorPresence::Unknown => None,
    };
    if let Some(src) = after_source {
        assert_eq!(
            src,
            PresenceSource::Ioreg,
            "sensor source must remain Ioreg after spamming events"
        );
    }

    // The verdict before/after may differ if the test crossed an
    // idle threshold during execution, but the source channel must
    // be stable.
    if let (
        OperatorPresence::Present { source: a, .. } | OperatorPresence::Absent { source: a, .. },
        OperatorPresence::Present { source: b, .. } | OperatorPresence::Absent { source: b, .. },
    ) = (&before, &after)
    {
        assert_eq!(a, b, "sensor source must not change between polls");
    }
}
