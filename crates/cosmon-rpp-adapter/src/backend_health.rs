// SPDX-License-Identifier: AGPL-3.0-only

//! `BackendHealthProbe` — read-only diagnostic endpoint
//! (T-V1-IFBDD-METER).
//!
//! Operators looking at the API surface need a quick answer to *"which
//! LLM backends does this adapter know about, and which one is
//! responsive?"* The probe is intentionally minimal:
//!
//! - In-RAM only — no persistence, no recovery state. The registry
//!   resets on adapter restart.
//! - Read-only — no auto-recovery, no failover, no circuit-breaker.
//!   The endpoint is a window onto whatever the wrapping
//!   `LlmBackend::complete` calls have observed.
//! - No probe is performed by this module *itself*; backend wrappers
//!   call [`BackendHealthRegistry::record`] at end-of-call. A
//!   configured-but-unused backend stays in the
//!   [`BackendStatus::ConfiguredButUnused`] state until a wrapper
//!   records its first probe.
//!
//! The framing is: we measure consumption *before* deciding policy.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status reported for a backend at a given snapshot.
///
/// Wire-format-stable: serialised as
/// `"ok" | "degraded" | "down" | "configured-but-unused"` so
/// dashboards and operator scripts can read the JSON without
/// re-parsing free-form labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendStatus {
    /// At least one recent probe succeeded.
    Ok,
    /// Recent probes show degraded latency or partial errors. The
    /// adapter does not act on this status — it is informational.
    Degraded,
    /// Recent probes failed.
    Down,
    /// The backend is in the operator's configured list but no
    /// probe has yet been recorded.
    ConfiguredButUnused,
}

/// One backend's health snapshot at the moment of the
/// `GET /health/backends` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendHealth {
    /// Backend identifier (`"anthropic"`, `"ollama"`, …) — same
    /// stringly-typed key used by `crate::routes::backend_health`
    /// and by `cosmon-state::token_meter`.
    pub name: String,
    /// Aggregated status — see [`BackendStatus`].
    pub status: BackendStatus,
    /// Wall-clock time of the most recent recorded probe (ms since
    /// the Unix epoch). `None` when no probe has yet been recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_ms: Option<i64>,
    /// 95th-percentile latency over the most recent observation
    /// window, in milliseconds. `None` when no probe has been
    /// recorded or the sample is below the minimum size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<u64>,
}

/// One probe observation recorded by an LLM-backend wrapper.
///
/// A `BackendProbe` is the *fact* — the registry reduces a stream of
/// probes to a [`BackendHealth`] snapshot at read time.
#[derive(Debug, Clone, Copy)]
pub struct BackendProbe {
    /// Wall-clock latency of the probed call in milliseconds.
    pub latency_ms: u64,
    /// Whether the call returned a 2xx-equivalent success.
    pub success: bool,
    /// When the probe was recorded.
    pub at: DateTime<Utc>,
}

/// Process-wide registry of backend health observations. Cheap to
/// share across `axum` handlers via `Arc<BackendHealthRegistry>`.
#[derive(Debug, Default)]
pub struct BackendHealthRegistry {
    /// Per-backend mutable observation list. Bounded ring buffer per
    /// backend so the in-RAM footprint cannot grow without bound
    /// even under sustained traffic.
    inner: Mutex<HashMap<String, BackendObservations>>,
}

/// Maximum probe samples retained per backend. Covers ~16 minutes of
/// once-per-second activity and keeps p95 statistically meaningful
/// under V0 traffic.
const SAMPLE_CAPACITY: usize = 1024;

#[derive(Debug, Default)]
struct BackendObservations {
    /// Most recent probes (FIFO, capped at [`SAMPLE_CAPACITY`]).
    samples: std::collections::VecDeque<BackendProbe>,
    /// Whether the operator has explicitly configured this backend.
    /// Configured backends without samples report
    /// `ConfiguredButUnused`; undeclared backends only become
    /// visible once a probe lands.
    configured: bool,
}

impl BackendHealthRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-register a list of backends from operator config so they
    /// are reported even before the first probe lands.
    pub fn register_configured(&self, names: impl IntoIterator<Item = String>) {
        if let Ok(mut g) = self.inner.lock() {
            for n in names {
                g.entry(n).or_default().configured = true;
            }
        }
    }

    /// Record one probe. Defensive — a poisoned mutex (which would
    /// only happen if a panic crossed the lock boundary) is ignored
    /// rather than re-panicking on the hot path.
    pub fn record(&self, backend: &str, probe: BackendProbe) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        let entry = g.entry(backend.to_owned()).or_default();
        if entry.samples.len() >= SAMPLE_CAPACITY {
            entry.samples.pop_front();
        }
        entry.samples.push_back(probe);
    }

    /// Take a sorted snapshot of every known backend.
    #[must_use]
    pub fn snapshot(&self) -> Vec<BackendHealth> {
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out: Vec<BackendHealth> = g
            .iter()
            .map(|(name, obs)| obs.summarise(name.clone()))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

impl BackendObservations {
    fn summarise(&self, name: String) -> BackendHealth {
        if self.samples.is_empty() {
            return BackendHealth {
                name,
                status: if self.configured {
                    BackendStatus::ConfiguredButUnused
                } else {
                    // A backend visible only because someone recorded
                    // a probe but with zero samples right now is
                    // a transient state (cleared registry?). Report
                    // unused so the wire shape stays predictable.
                    BackendStatus::ConfiguredButUnused
                },
                last_check_ms: None,
                latency_p95_ms: None,
            };
        }

        // Last observation drives `last_check_ms`.
        let last = self.samples.back().copied().expect("non-empty");
        let last_check_ms = Some(last.at.timestamp_millis());

        // Recent failure ratio decides the status.
        let total = self.samples.len();
        let failures = self.samples.iter().filter(|p| !p.success).count();
        let status = if failures == 0 {
            BackendStatus::Ok
        } else if failures * 2 >= total {
            BackendStatus::Down
        } else {
            BackendStatus::Degraded
        };

        // p95 over successful probes only — failed calls usually
        // short-circuit and would skew latency downwards.
        let mut latencies: Vec<u64> = self
            .samples
            .iter()
            .filter(|p| p.success)
            .map(|p| p.latency_ms)
            .collect();
        let latency_p95_ms = if latencies.len() >= 5 {
            latencies.sort_unstable();
            let idx = (latencies.len() as f64 * 0.95).ceil() as usize - 1;
            Some(latencies[idx.min(latencies.len() - 1)])
        } else {
            None
        };

        BackendHealth {
            name,
            status,
            last_check_ms,
            latency_p95_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(success: bool, latency_ms: u64) -> BackendProbe {
        BackendProbe {
            success,
            latency_ms,
            at: Utc::now(),
        }
    }

    #[test]
    fn configured_backend_without_probe_reports_unused() {
        let r = BackendHealthRegistry::new();
        r.register_configured(["anthropic".to_owned(), "ollama".to_owned()]);
        let snap = r.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].name, "anthropic");
        assert_eq!(snap[0].status, BackendStatus::ConfiguredButUnused);
        assert!(snap[0].last_check_ms.is_none());
    }

    #[test]
    fn probe_marks_backend_ok_and_records_last_check() {
        let r = BackendHealthRegistry::new();
        r.record("anthropic", probe(true, 200));
        let snap = r.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].status, BackendStatus::Ok);
        assert!(snap[0].last_check_ms.is_some());
    }

    #[test]
    fn full_failures_yield_down() {
        let r = BackendHealthRegistry::new();
        for _ in 0..3 {
            r.record("openai", probe(false, 0));
        }
        let snap = r.snapshot();
        assert_eq!(snap[0].status, BackendStatus::Down);
    }

    #[test]
    fn partial_failures_yield_degraded() {
        let r = BackendHealthRegistry::new();
        for _ in 0..7 {
            r.record("ollama", probe(true, 100));
        }
        for _ in 0..2 {
            r.record("ollama", probe(false, 0));
        }
        let snap = r.snapshot();
        assert_eq!(snap[0].status, BackendStatus::Degraded);
    }

    #[test]
    fn p95_only_emitted_after_minimum_sample() {
        let r = BackendHealthRegistry::new();
        r.record("anthropic", probe(true, 100));
        r.record("anthropic", probe(true, 200));
        let snap = r.snapshot();
        assert!(
            snap[0].latency_p95_ms.is_none(),
            "below minimum sample size — p95 must stay None"
        );
        for ms in [100, 110, 200, 250, 300] {
            r.record("anthropic", probe(true, ms));
        }
        let snap = r.snapshot();
        assert!(snap[0].latency_p95_ms.is_some());
    }

    #[test]
    fn ring_buffer_caps_in_ram_footprint() {
        let r = BackendHealthRegistry::new();
        for ms in 0..(SAMPLE_CAPACITY as u64 + 16) {
            r.record("anthropic", probe(true, ms));
        }
        // Snapshot succeeds and reports a status — the registry
        // must not grow unbounded under sustained probing.
        let snap = r.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].status, BackendStatus::Ok);
    }

    #[test]
    fn snapshot_is_sorted_by_name_for_stable_diffing() {
        let r = BackendHealthRegistry::new();
        r.register_configured([
            "ollama".to_owned(),
            "anthropic".to_owned(),
            "openai".to_owned(),
        ]);
        let names: Vec<_> = r.snapshot().into_iter().map(|h| h.name).collect();
        assert_eq!(names, vec!["anthropic", "ollama", "openai"]);
    }
}
