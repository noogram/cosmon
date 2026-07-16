// SPDX-License-Identifier: AGPL-3.0-only

//! Operator-presence sensor *adapters* — concrete, process-spawning sensors.
//!
//! The [`PresenceSensor`](cosmon_core::presence_sensor::PresenceSensor) port
//! and its readout types live in `cosmon-core`. The OS-backed implementations
//! that actually spawn a binary (`ioreg`, and `who`/`utmp` in the future)
//! live here, in an adapter crate, so the domain crate performs no process
//! I/O (INV-DOMAIN-PURE-NO-IO, ADR-082).
//!
//! The no-cloning discipline documented on the core trait is enforced by
//! [`coupling_lint`] — a source-text scan over every shipped sensor impl. It
//! is a **coupling lint**, not a purity audit: it proves a sensor reads from
//! a substrate cosmon does not write (it bans `.cosmon/state` reads and
//! direct `File::open`), while deliberately permitting the `ioreg` process
//! spawn the sensor is built around.

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::IoregSensor;

mod coupling_lint;
