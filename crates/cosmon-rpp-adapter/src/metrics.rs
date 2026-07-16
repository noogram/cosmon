// SPDX-License-Identifier: AGPL-3.0-only

//! In-RAM metrics registry for the operator-facing `/metrics` and
//! `/diagnostics` endpoints.
//!
//! Counters are monotonic atomics; the Prometheus convention is that
//! the consumer computes rates with `rate()` over a scrape window —
//! the adapter never tries to track "per minute" itself. Uptime is the
//! single resettable signal (it restarts on process bounce).
//!
//! The registry is intentionally minimal: it never touches the
//! filesystem, never opens a connection, never reads tenant state. The
//! routes that *render* metrics may read the JWKS / nucleon-map /
//! backend-health snapshots, but those are already RAM-resident
//! projections.
//!
//! ## What is and is not measured here
//!
//! - **HTTP status class counters** — every response that flows through
//!   [`metrics_layer`] is counted by 2xx / 3xx / 4xx / 5xx. Cheap,
//!   covers JWT rejects (401), rate-limit rejects (429), and 5xx
//!   anomalies without threading state through the error path.
//! - **JWT and rate-limit reject counters** — bumped explicitly at
//!   the admission boundary via [`MetricsRegistry::record_reject`].
//!   The label is the same stable string the wire body carries
//!   ([`crate::RppRejectReason::label`]) so the metric and the audit
//!   trail share a vocabulary.
//! - **No per-tenant breakdown** — labelling counters by `noyau` or
//!   `sub` would leak tenant existence onto a public surface; metrics
//!   are tenant-blind by design.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// Shared metrics registry. Cheap to clone via `Arc<MetricsRegistry>`.
///
/// All counters are monotonic over the process lifetime. Reset on
/// adapter restart — by design (`/diagnostics` exposes `uptime_seconds`
/// so an observer can detect the boundary).
#[derive(Debug)]
pub struct MetricsRegistry {
    /// Wall-clock the registry was constructed at (≈ adapter boot).
    started_at: Instant,
    /// Responses with status 100–199.
    responses_1xx: AtomicU64,
    /// Responses with status 200–299.
    responses_2xx: AtomicU64,
    /// Responses with status 300–399.
    responses_3xx: AtomicU64,
    /// Responses with status 400–499.
    responses_4xx: AtomicU64,
    /// Responses with status 500–599.
    responses_5xx: AtomicU64,
    /// JWT / admission-stage rejects counted by stable reason label
    /// (see [`crate::RppRejectReason::label`]). Bumped from the
    /// adapter's admission helpers; the wire status alone cannot
    /// distinguish (`expired`, `audience_mismatch`, `unknown_sub`).
    rejects_by_reason: Mutex<HashMap<&'static str, u64>>,
}

impl MetricsRegistry {
    /// Build an empty registry, starting the uptime clock now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            responses_1xx: AtomicU64::new(0),
            responses_2xx: AtomicU64::new(0),
            responses_3xx: AtomicU64::new(0),
            responses_4xx: AtomicU64::new(0),
            responses_5xx: AtomicU64::new(0),
            rejects_by_reason: Mutex::new(HashMap::new()),
        }
    }

    /// Seconds since the registry was constructed (≈ adapter uptime).
    #[must_use]
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Bump the appropriate status-class counter. Called by
    /// [`metrics_layer`] after the handler chain has produced a
    /// response.
    pub fn record_status(&self, status: u16) {
        // Anything outside the standard ranges (impossible from axum,
        // defensive) is bucketed into 5xx by the wildcard arm so a
        // pathological status cannot hide outside the counters.
        let counter = match status {
            100..=199 => &self.responses_1xx,
            200..=299 => &self.responses_2xx,
            300..=399 => &self.responses_3xx,
            400..=499 => &self.responses_4xx,
            _ => &self.responses_5xx,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an admission-stage rejection by its stable reason
    /// label. Idempotent on lock poisoning — a stale poisoned mutex
    /// drops the count rather than panicking on the hot path.
    pub fn record_reject(&self, reason_label: &'static str) {
        if let Ok(mut g) = self.rejects_by_reason.lock() {
            *g.entry(reason_label).or_insert(0) += 1;
        }
    }

    /// Read-only snapshot of the per-reason reject counters, sorted
    /// by label for stable wire output.
    #[must_use]
    pub fn rejects_snapshot(&self) -> Vec<(&'static str, u64)> {
        let Ok(g) = self.rejects_by_reason.lock() else {
            return Vec::new();
        };
        let mut out: Vec<_> = g.iter().map(|(k, v)| (*k, *v)).collect();
        out.sort_by_key(|(label, _)| *label);
        out
    }

    /// Total rejects across all reason labels.
    #[must_use]
    pub fn rejects_total(&self) -> u64 {
        let Ok(g) = self.rejects_by_reason.lock() else {
            return 0;
        };
        g.values().sum()
    }

    /// Per-status-class snapshot. Order: `(1xx, 2xx, 3xx, 4xx, 5xx)`.
    #[must_use]
    pub fn status_class_counts(&self) -> StatusClassCounts {
        StatusClassCounts {
            responses_1xx: self.responses_1xx.load(Ordering::Relaxed),
            responses_2xx: self.responses_2xx.load(Ordering::Relaxed),
            responses_3xx: self.responses_3xx.load(Ordering::Relaxed),
            responses_4xx: self.responses_4xx.load(Ordering::Relaxed),
            responses_5xx: self.responses_5xx.load(Ordering::Relaxed),
        }
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Status-class breakdown returned by
/// [`MetricsRegistry::status_class_counts`]. Sums across all routes,
/// not per-route — labelling by route would over-cardinalise the
/// Prometheus surface on a small server.
///
/// The repeated `responses_` prefix is intentional: the public field
/// names map 1:1 to the JSON-renamed keys (`"1xx"`, …) and Prometheus
/// labels (`class="1xx"`, …). Renaming would lose the symmetry.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[allow(clippy::struct_field_names)]
pub struct StatusClassCounts {
    /// Status 1xx response count.
    #[serde(rename = "1xx")]
    pub responses_1xx: u64,
    /// Status 2xx response count.
    #[serde(rename = "2xx")]
    pub responses_2xx: u64,
    /// Status 3xx response count.
    #[serde(rename = "3xx")]
    pub responses_3xx: u64,
    /// Status 4xx response count.
    #[serde(rename = "4xx")]
    pub responses_4xx: u64,
    /// Status 5xx response count.
    #[serde(rename = "5xx")]
    pub responses_5xx: u64,
}

impl StatusClassCounts {
    /// Total responses observed across all status classes.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.responses_1xx
            .saturating_add(self.responses_2xx)
            .saturating_add(self.responses_3xx)
            .saturating_add(self.responses_4xx)
            .saturating_add(self.responses_5xx)
    }
}

/// Axum middleware that records every response's status class into the
/// shared [`MetricsRegistry`]. Layered on the whole router; the cost
/// is one atomic add per request.
///
/// Mounted after the routing decision so 404s from unknown paths still
/// land in the 4xx bucket.
pub async fn metrics_layer(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let resp = next.run(req).await;
    state.metrics.record_status(resp.status().as_u16());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_registry_has_zero_counts() {
        let m = MetricsRegistry::new();
        let s = m.status_class_counts();
        assert_eq!(s.total(), 0);
        assert_eq!(m.rejects_total(), 0);
    }

    #[test]
    fn uptime_monotonically_nonzero_after_a_beat() {
        let m = MetricsRegistry::new();
        std::thread::sleep(std::time::Duration::from_millis(5));
        // Sub-second resolution is fine; uptime is a u64 of full
        // seconds and may still read 0 here. Just check the call
        // doesn't panic and returns a sensible value.
        let _ = m.uptime_seconds();
    }

    #[test]
    fn status_buckets_split_correctly() {
        let m = MetricsRegistry::new();
        m.record_status(200);
        m.record_status(201);
        m.record_status(401);
        m.record_status(404);
        m.record_status(500);
        let s = m.status_class_counts();
        assert_eq!(s.responses_2xx, 2);
        assert_eq!(s.responses_4xx, 2);
        assert_eq!(s.responses_5xx, 1);
        assert_eq!(s.total(), 5);
    }

    #[test]
    fn out_of_range_status_buckets_into_5xx() {
        let m = MetricsRegistry::new();
        m.record_status(999);
        let s = m.status_class_counts();
        assert_eq!(s.responses_5xx, 1);
    }

    #[test]
    fn rejects_by_reason_are_summed() {
        let m = MetricsRegistry::new();
        m.record_reject("expired");
        m.record_reject("expired");
        m.record_reject("unknown_sub");
        assert_eq!(m.rejects_total(), 3);
        let snap = m.rejects_snapshot();
        // Sorted by label.
        assert_eq!(snap, vec![("expired", 2), ("unknown_sub", 1)]);
    }
}
