// SPDX-License-Identifier: AGPL-3.0-only

//! The host reading printed alongside `cs ensemble --cluster`
//! (noogram/cosmon #58, stage 1, Child C).
//!
//! # This module reports and never refuses
//!
//! It renders observations: what the kernel says about the machine, and how
//! much cosmon work the galaxies on it currently account for. There is no
//! threshold here, no classification, and no verdict. The decision half of
//! issue #58 is [`cosmon_core::admission::decide_admission`], it is wired to
//! nothing, and stage 3 is where a refusal may appear. A surface that coloured
//! a number red would be asserting a cause it did not measure — which is the
//! exact mistake the measurement behind this issue could not avoid, and must
//! not be repeated in the product.
//!
//! The kernel's own pressure level is carried through **verbatim**, as the
//! integer the platform publishes. cosmon does not own that scale and does not
//! translate it.
//!
//! # One value, two projections
//!
//! Human and JSON output are both built from [`MachineReading::observations`],
//! a single ordered list of `(key, json value, rendered text)` triples. Neither
//! renderer reads the snapshot directly, so a field cannot be added to one and
//! forgotten in the other — the parity test in this module pins that, and it
//! is a test that can actually go red, because a renderer that hard-coded a
//! field would fail it.
//!
//! # `None` is printed as unavailable, never as zero
//!
//! Restated from [`cosmon_core::admission`] because this is the module a
//! reader's eyes land on: an unread counter renders as the word `unavailable`
//! and serialises as JSON `null`, and its key is additionally listed under
//! `unavailable`. Printing `0` for "I could not see" would render a blind
//! probe as a perfectly healthy machine, which is the single failure this
//! surface exists to prevent.
//!
//! # The harvest blind spot, and what the counts therefore cover
//!
//! Counting live workers alone reproduces a known blind spot. While
//! `cs done <molecule>` runs, the worker is already torn down and the molecule
//! is `completed`, yet the post-merge gate sweep — a full workspace
//! `cargo check`, the heaviest compile burst of the cycle — is still running in
//! the main checkout. A worker count prints a quiet machine at the loudest
//! moment.
//!
//! Durable evidence of that state already exists and no new artifact is
//! created here: `cs done` holds the galaxy's **trunk lock** across the merge,
//! the frontier write, the post-merge hook and the archive write, and stamps
//! `<state_dir>/trunk.lock` with the holding PID and command
//! ([`cosmon_filestore::read_trunk_lock_holder_at`]). A non-empty hint is a
//! harvest in flight. This module reads it; it writes nothing, and creates
//! nothing in the user-level cosmon root.
//!
//! The residual is named rather than hidden: a `cs done` killed with `SIGKILL`
//! never clears its hint, so the PID is checked against the OS through
//! [`cosmon_process_witness::process_start_time`] and a hint whose process is
//! gone is reported as stale rather than counted. A hint with no PID at all is
//! counted, because the bias on this surface runs toward reporting activity,
//! never toward an unearned "idle".
//!
//! [`MachineReading::COUNT_SCOPE`] states in one sentence what the counts do
//! and do not cover, and it is printed with them. A number that silently omits
//! a category is worse than a number that names its own scope.

use std::fmt::Write as _;
use std::path::Path;

use cosmon_core::admission::MachineSnapshot;
use serde_json::{json, Value};

/// One reported counter, in both projections at once.
///
/// The invariant this type exists to hold: `json` and `human` are produced
/// from the same value at the same instant, so the two renderers cannot
/// disagree about what was measured.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Observation {
    /// The field name, carrying its unit (`total_memory_bytes`, not
    /// `total_memory`). Units live in the key so no reader has to guess at
    /// the scale of a bare integer.
    pub key: &'static str,
    /// The JSON projection: the number, or [`Value::Null`] for an unread
    /// counter. Never `0` for unread — see the module doc.
    pub json: Value,
    /// The human projection: the number with its unit spelled out, or the
    /// literal word `unavailable`.
    pub human: String,
}

impl Observation {
    /// Whether this counter could not be read.
    fn is_unavailable(&self) -> bool {
        self.json.is_null()
    }
}

/// Machine-wide cosmon accounting, derived from state each galaxy already
/// owns.
///
/// A **reader**: every field is summed from files the galaxies write for their
/// own reasons (`fleet.json`, molecule `state.json`, `trunk.lock`). Nothing
/// here is persisted, and no machine-scoped artifact is created — that would be
/// a new shared writer and would require an ADR (ADR-052 one-writer
/// discipline), which stage 1 deliberately does not take on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CosmonActivity {
    /// Galaxies discovered under the scanned cluster root.
    pub galaxies: usize,
    /// Workers registered across those galaxies' fleets, summed.
    pub registered_workers: usize,
    /// Molecules recorded `running` across those galaxies, summed.
    pub running_molecules: usize,
    /// Harvests holding a galaxy's trunk lock right now — the category a
    /// worker count misses entirely. See the module doc.
    pub harvests_in_flight: usize,
    /// Trunk-lock hints whose recorded process is gone: a harvest that was
    /// killed without clearing its hint. Reported separately so it is neither
    /// counted as running work nor silently dropped.
    pub stale_harvest_hints: usize,
}

/// Evidence that one galaxy is mid-harvest, read from its trunk lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HarvestWitness {
    /// The `cs` command recorded as holding the lock.
    pub cmd: Option<String>,
    /// The holder's PID, when the hint recorded one.
    pub pid: Option<u32>,
    /// When the lock was taken, RFC-3339, as written.
    pub started_at: Option<String>,
    /// Whether the recorded process still exists. `true` when no PID was
    /// recorded at all: an unverifiable hint is reported as activity rather
    /// than dismissed, because the cost of the two errors is not symmetric.
    pub live: bool,
}

/// Read the trunk lock under `state_dir` and say whether a harvest is in
/// flight there.
///
/// [`None`] means the lock file is absent or the hint is empty — the state
/// `cs done` leaves behind on release. No lock is taken: asking "is the lock
/// held?" by acquiring it would make this reader wait for the very harvest it
/// is reporting.
pub(crate) fn harvest_witness(state_dir: &Path) -> Option<HarvestWitness> {
    let holder = cosmon_filestore::read_trunk_lock_holder_at(state_dir)?;
    let live = holder
        .pid
        .is_none_or(|pid| cosmon_process_witness::process_start_time(pid).is_some());
    Some(HarvestWitness {
        cmd: holder.cmd,
        pid: holder.pid,
        started_at: holder.started_at,
        live,
    })
}

/// One host snapshot plus the machine-wide cosmon counts, in the shape both
/// renderers read.
#[derive(Debug, Clone)]
pub(crate) struct MachineReading {
    /// The reading, or the reason there is none. `Err` is a probe that
    /// produced nothing at all; a *partial* reading is `Ok` with `None`
    /// counters, which is a different and far more common case.
    host: Result<MachineSnapshot, String>,
    /// What cosmon itself accounts for on this machine.
    activity: CosmonActivity,
}

impl MachineReading {
    /// What the counts cover, and what they do not.
    ///
    /// Printed and serialised beside the numbers so the surface cannot be read
    /// as "the machine is idle" when it only means "no cosmon galaxy under
    /// this root reports work".
    pub const COUNT_SCOPE: &'static str = "counts cover cosmon galaxies under the scanned cluster \
         root only: workers each galaxy has registered, molecules it records as running, and \
         harvests holding its trunk lock (which covers the post-merge gate sweep, when no worker \
         is alive). Compute started outside a cosmon galaxy is not counted.";

    /// Build a reading from a probe result and the counts derived from the
    /// galaxies already scanned.
    pub fn new(host: Result<MachineSnapshot, String>, activity: CosmonActivity) -> Self {
        Self { host, activity }
    }

    /// Why there is no host reading, if there is none.
    pub fn probe_error(&self) -> Option<&str> {
        self.host.as_ref().err().map(String::as_str)
    }

    /// The single ordered list both projections are built from.
    ///
    /// A probe that failed entirely yields the same keys with every value
    /// [`Value::Null`]: the shape of the document does not depend on whether
    /// the host could be read, so a consumer never has to branch on a missing
    /// field.
    pub fn observations(&self) -> Vec<Observation> {
        let snapshot = self.host.as_ref().ok();
        let bytes = |key: &'static str, value: Option<u64>| Observation {
            key,
            json: value.map_or(Value::Null, |v| json!(v)),
            human: value.map_or_else(unavailable, render_bytes),
        };
        let count = |key: &'static str, value: Option<u32>, unit: &str| Observation {
            key,
            json: value.map_or(Value::Null, |v| json!(v)),
            human: value.map_or_else(unavailable, |v| format!("{v} {unit}")),
        };
        let load = |key: &'static str, index: usize| Observation {
            key,
            json: snapshot
                .and_then(|s| s.load_average)
                .map_or(Value::Null, |l| json!(l[index])),
            human: snapshot
                .and_then(|s| s.load_average)
                .map_or_else(unavailable, |l| format!("{:.2} (runnable tasks)", l[index])),
        };

        vec![
            bytes(
                "total_memory_bytes",
                snapshot.and_then(|s| s.total_memory_bytes),
            ),
            bytes(
                "available_memory_bytes",
                snapshot.and_then(|s| s.available_memory_bytes),
            ),
            bytes("used_swap_bytes", snapshot.and_then(|s| s.used_swap_bytes)),
            bytes(
                "total_swap_bytes",
                snapshot.and_then(|s| s.total_swap_bytes),
            ),
            count(
                "kernel_memory_pressure_level",
                snapshot.and_then(|s| s.kernel_memory_pressure_level),
                "(kernel-reported, uninterpreted)",
            ),
            count(
                "logical_cpu_count",
                snapshot.and_then(|s| s.logical_cpu_count),
                "logical CPUs",
            ),
            load("load_average_1m", 0),
            load("load_average_5m", 1),
            load("load_average_15m", 2),
        ]
    }

    /// The JSON projection — the `machine` object of `cs ensemble --cluster
    /// --json`.
    pub fn to_json(&self) -> Value {
        let observations = self.observations();
        let mut host = serde_json::Map::new();
        for obs in &observations {
            host.insert(obs.key.to_owned(), obs.json.clone());
        }
        host.insert(
            "unavailable".to_owned(),
            json!(observations
                .iter()
                .filter(|o| o.is_unavailable())
                .map(|o| o.key)
                .collect::<Vec<_>>()),
        );
        host.insert(
            "sampled_at".to_owned(),
            self.host
                .as_ref()
                .map_or(Value::Null, |s| json!(s.sampled_at.to_rfc3339())),
        );
        host.insert(
            "probe_error".to_owned(),
            self.probe_error().map_or(Value::Null, |e| json!(e)),
        );

        json!({
            "host": host,
            "cosmon_activity": {
                "galaxies": self.activity.galaxies,
                "registered_workers": self.activity.registered_workers,
                "running_molecules": self.activity.running_molecules,
                "harvests_in_flight": self.activity.harvests_in_flight,
                "stale_harvest_hints": self.activity.stale_harvest_hints,
                "scope": Self::COUNT_SCOPE,
            },
        })
    }

    /// The human projection, as a block of `key: value` lines.
    ///
    /// Returned as a `String` rather than printed so it is testable without
    /// capturing stdout; the command prints what this returns.
    pub fn to_human(&self) -> String {
        let mut out = String::new();
        out.push_str("Machine reading (observations only — no threshold is applied):\n");
        // `write!` into a `String` is infallible; the `let _` is the idiom this
        // workspace uses rather than an `expect` on a Result that cannot be Err.
        if let Some(err) = self.probe_error() {
            let _ = writeln!(out, "  host reading unavailable: {err}");
        }
        for obs in self.observations() {
            let _ = writeln!(out, "  {}: {}", obs.key, obs.human);
        }
        let _ = write!(
            out,
            "  galaxies: {}\n  registered_workers: {}\n  running_molecules: {}\n  \
             harvests_in_flight: {}\n  stale_harvest_hints: {}\n  scope: {}\n",
            self.activity.galaxies,
            self.activity.registered_workers,
            self.activity.running_molecules,
            self.activity.harvests_in_flight,
            self.activity.stale_harvest_hints,
            Self::COUNT_SCOPE,
        );
        out
    }
}

/// The rendering of a counter that could not be read. One spelling, in one
/// place, so no branch can drift into printing `0`.
fn unavailable() -> String {
    "unavailable (counter not read)".to_owned()
}

/// Render a byte count with its unit stated twice: exactly, and in the scale a
/// human reads. The exact figure comes first so a script that greps the line
/// never picks up the rounded one.
fn render_bytes(value: u64) -> String {
    // Lossy above 2^53, which is 8 PiB — beyond any host this reads, and the
    // exact integer is printed alongside regardless.
    #[allow(clippy::cast_precision_loss)]
    let gib = value as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{value} bytes ({gib:.2} GiB)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_764_000_000, 0).unwrap_or_default()
    }

    fn full_snapshot() -> MachineSnapshot {
        let mut s = MachineSnapshot::unread_at(at());
        s.total_memory_bytes = Some(137_438_953_472);
        s.available_memory_bytes = Some(78_859_059_200);
        s.total_swap_bytes = Some(3_221_225_472);
        s.used_swap_bytes = Some(1_611_137_024);
        s.kernel_memory_pressure_level = Some(1);
        s.logical_cpu_count = Some(16);
        s.load_average = Some([6.38, 6.21, 5.38]);
        s
    }

    /// Falsifier 1 — known byte counts reach `--json` with unambiguous units.
    #[test]
    fn known_byte_counts_reach_json_with_units() {
        let reading = MachineReading::new(Ok(full_snapshot()), CosmonActivity::default());
        let host = &reading.to_json()["host"];

        assert_eq!(host["total_memory_bytes"], json!(137_438_953_472_u64));
        assert_eq!(host["available_memory_bytes"], json!(78_859_059_200_u64));
        assert_eq!(host["used_swap_bytes"], json!(1_611_137_024_u64));
        assert_eq!(host["total_swap_bytes"], json!(3_221_225_472_u64));
        assert_eq!(host["logical_cpu_count"], json!(16));
        assert_eq!(host["kernel_memory_pressure_level"], json!(1));
        assert_eq!(host["load_average_1m"], json!(6.38));
        assert_eq!(host["unavailable"], json!([] as [&str; 0]));
        // The unit is in the key, so no consumer has to infer the scale of a
        // bare integer: every byte counter says `_bytes`.
        for key in [
            "total_memory_bytes",
            "available_memory_bytes",
            "used_swap_bytes",
            "total_swap_bytes",
        ] {
            assert!(host.get(key).is_some(), "{key} must be present");
        }
    }

    /// Falsifier 2, the load-bearing one — an unread counter is `null` and is
    /// named, never `0`. A zero here reads as a healthy machine.
    #[test]
    fn an_unread_counter_is_explicitly_unavailable_never_zero() {
        let mut partial = full_snapshot();
        partial.used_swap_bytes = None;
        partial.available_memory_bytes = None;
        let reading = MachineReading::new(Ok(partial), CosmonActivity::default());

        let host = &reading.to_json()["host"];
        assert_eq!(host["used_swap_bytes"], Value::Null);
        assert_eq!(host["available_memory_bytes"], Value::Null);
        assert_ne!(host["used_swap_bytes"], json!(0));
        assert_eq!(
            host["unavailable"],
            json!(["available_memory_bytes", "used_swap_bytes"])
        );
        // The counters that *were* read are untouched by a neighbour's failure.
        assert_eq!(host["total_memory_bytes"], json!(137_438_953_472_u64));

        let human = reading.to_human();
        assert!(
            human.contains("used_swap_bytes: unavailable"),
            "human output must say unavailable: {human}"
        );
        assert!(
            !human.contains("used_swap_bytes: 0"),
            "an unread counter must never render as zero: {human}"
        );
    }

    /// Falsifier 3 (the projection half) — a probe that failed entirely still
    /// yields the full document shape, with every counter null and the reason
    /// carried. The command's stderr line is asserted in the CLI test.
    #[test]
    fn a_failed_probe_still_renders_and_says_why() {
        let reading = MachineReading::new(
            Err("machine probe unsupported on this platform: none".to_owned()),
            CosmonActivity {
                galaxies: 2,
                registered_workers: 5,
                running_molecules: 3,
                harvests_in_flight: 1,
                stale_harvest_hints: 0,
            },
        );

        let value = reading.to_json();
        let host = &value["host"];
        assert_eq!(host["total_memory_bytes"], Value::Null);
        assert_eq!(host["sampled_at"], Value::Null);
        assert!(host["probe_error"]
            .as_str()
            .is_some_and(|e| e.contains("unsupported")));
        assert_eq!(host["unavailable"].as_array().map(Vec::len), Some(9));
        // The counts are independent of the probe: they came from the galaxies.
        assert_eq!(value["cosmon_activity"]["registered_workers"], json!(5));
        assert!(reading.to_human().contains("host reading unavailable"));
    }

    /// Falsifier 5 — the two projections agree field by field. A renderer
    /// updated alone fails here: the parity is over the *same* key list, and a
    /// hard-coded field in either would not appear in the other.
    #[test]
    fn human_and_json_agree_field_by_field() {
        let reading = MachineReading::new(
            Ok(full_snapshot()),
            CosmonActivity {
                galaxies: 2,
                registered_workers: 7,
                running_molecules: 4,
                harvests_in_flight: 1,
                stale_harvest_hints: 2,
            },
        );
        let human = reading.to_human();
        let value = reading.to_json();
        let host = value["host"].as_object().expect("host is an object");

        for obs in reading.observations() {
            assert_eq!(
                host.get(obs.key),
                Some(&obs.json),
                "JSON must carry {}",
                obs.key
            );
            assert!(
                human.contains(&format!("{}: {}", obs.key, obs.human)),
                "human output must carry {}: {human}",
                obs.key
            );
            // And the raw number itself survives into the human line, so the
            // two are not merely both present but the same measurement.
            if let Some(n) = obs.json.as_u64() {
                assert!(
                    human.contains(&n.to_string()),
                    "human output must quote {n} for {}",
                    obs.key
                );
            }
        }
        // Every JSON host key is accounted for by an observation, so a field
        // added to `to_json` alone fails this test rather than silently
        // shipping in one projection.
        let observed: Vec<&str> = reading.observations().iter().map(|o| o.key).collect();
        for key in host.keys() {
            assert!(
                observed.contains(&key.as_str())
                    || ["unavailable", "sampled_at", "probe_error"].contains(&key.as_str()),
                "JSON key {key} has no observation behind it"
            );
        }
        // The activity counts appear in both projections too.
        for (key, want) in [
            ("registered_workers", 7),
            ("running_molecules", 4),
            ("harvests_in_flight", 1),
            ("stale_harvest_hints", 2),
            ("galaxies", 2),
        ] {
            assert_eq!(value["cosmon_activity"][key], json!(want));
            assert!(
                human.contains(&format!("{key}: {want}")),
                "human output must carry {key}"
            );
        }
    }

    /// The scope label is not decorative: it is printed and serialised, and it
    /// names the category the count would otherwise silently omit.
    #[test]
    fn the_count_states_its_own_scope() {
        let reading = MachineReading::new(Ok(full_snapshot()), CosmonActivity::default());
        let scope = MachineReading::COUNT_SCOPE;
        assert!(scope.contains("post-merge gate sweep"));
        assert!(scope.contains("not counted"));
        assert_eq!(reading.to_json()["cosmon_activity"]["scope"], json!(scope));
        assert!(reading.to_human().contains(scope));
    }

    /// A live trunk-lock hint is a harvest in flight; an empty file is not.
    /// This is the harvest-blind-spot falsifier at the witness level.
    #[test]
    fn a_live_trunk_lock_hint_is_a_harvest_in_flight() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(
            harvest_witness(tmp.path()),
            None,
            "no lock file, no harvest"
        );

        // Released: the guard truncates the hint, so an empty file is *not* a
        // harvest. Without this case a released lock would inflate the count
        // forever.
        std::fs::write(tmp.path().join("trunk.lock"), "").expect("write");
        assert_eq!(harvest_witness(tmp.path()), None, "empty hint, no harvest");

        // Held by a process that certainly exists: this one.
        let pid = std::process::id();
        std::fs::write(
            tmp.path().join("trunk.lock"),
            format!("pid={pid}\ncmd=cs done task-20260906-73be\nstarted_at=2026-09-06T10:00:00Z\nhost=h\n"),
        )
        .expect("write");
        let witness = harvest_witness(tmp.path()).expect("a stamped hint is a harvest");
        assert!(witness.live);
        assert_eq!(witness.pid, Some(pid));
        assert_eq!(witness.cmd.as_deref(), Some("cs done task-20260906-73be"));
    }

    /// A hint left by a killed harvest is reported stale, not counted as
    /// running work — and not thrown away either.
    #[test]
    fn a_hint_whose_process_is_gone_is_stale() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // PID 0 is never a live user process on the platforms this runs on;
        // `process_start_time` returns None for it.
        std::fs::write(tmp.path().join("trunk.lock"), "pid=0\ncmd=cs done gone\n").expect("write");
        let witness = harvest_witness(tmp.path()).expect("a stamped hint is read");
        assert!(!witness.live, "a vanished holder is not in flight");

        // A hint with no PID cannot be checked, and is reported as activity:
        // the two errors do not cost the same, and an unearned "idle" is the
        // expensive one.
        std::fs::write(tmp.path().join("trunk.lock"), "cmd=cs done unknown-pid\n").expect("write");
        let witness = harvest_witness(tmp.path()).expect("a stamped hint is read");
        assert!(witness.live, "an unverifiable hint counts as in flight");
        assert_eq!(witness.pid, None);
    }
}
