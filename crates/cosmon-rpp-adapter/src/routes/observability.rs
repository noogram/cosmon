// SPDX-License-Identifier: AGPL-3.0-only

//! `/metrics` and `/diagnostics` handlers.
//!
//! Both routes are *operational* — outside `/v1/`, no JWT gate, and
//! deliberately excluded from the §8p frozen API surface. They are not
//! tenant-facing data: `/metrics` produces Prometheus text for an
//! operator scrape; `/diagnostics` produces a JSON snapshot of
//! in-RAM projections (JWKS, nucleon map, backend health, event bus).
//!
//! ## Why two endpoints, not one
//!
//! - Prometheus expects a `text/plain; version=0.0.4` body with a
//!   strict line-oriented grammar. Wrapping it in JSON would prevent
//!   stock scrapers (`prometheus-server`, `vmagent`, `grafana-agent`)
//!   from ingesting it.
//! - `/diagnostics` is the human-readable / dashboard side: the same
//!   counters appear, plus structured projections that do not fit the
//!   metric model (per-issuer JWKS key counts, configured backend
//!   list, install templating fingerprint, …).
//!
//! `/healthz` stays minimal-plus-version — a probe-friendly four-key
//! body (`ok`, `service`, `version`, `api_surface_version`) — so
//! liveness checks remain allocation-bounded.
//!
//! ## Authentication
//!
//! Both endpoints are intentionally unauthenticated. They live behind
//! the same operator perimeter as `/healthz` and `/health/backends`
//! (typically a Tailscale-restricted host) and never leak per-tenant
//! data — only aggregate counters and projection sizes.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Json};
use serde::Serialize;
use serde_json::{json, Value};

use crate::AppState;

/// Content-Type emitted by [`metrics_handler`]. Matches the Prometheus
/// exposition format `text/plain; version=0.0.4; charset=utf-8`.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics` — Prometheus exposition.
///
/// The format is deliberately conservative: one `# HELP` + one `# TYPE`
/// comment per metric family, then the samples. Labels are quoted and
/// limited to small enums (status class, backend name, issuer) so the
/// cardinality of the scrape stays bounded.
pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let body = render_prometheus(&state);
    ([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], body)
}

// The Prometheus body is essentially a flat sequence of `# HELP` /
// `# TYPE` / sample lines — splitting it into smaller helpers would
// add ceremony without clarity (the metric set is the function).
#[allow(clippy::too_many_lines)]
fn render_prometheus(state: &AppState) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(4096);
    let uptime = state.metrics.uptime_seconds();
    let statuses = state.metrics.status_class_counts();
    let rejects = state.metrics.rejects_snapshot();
    let backends = state.backend_health.snapshot();
    let nucleon_map = state.nucleon_map.load();
    let bindings = nucleon_map.binding_count();
    let noyaux = nucleon_map.noyaux().len();
    let jwks_per_iss = state.jwks.load().key_counts_by_issuer();
    let subscribers = state.events.receiver_count();
    let posture = match state.posture {
        crate::Posture::Active => "active",
        crate::Posture::Prepared => "prepared",
    };

    // Uptime — a gauge so a scrape that misses the boot still sees the
    // current age, and a `reset()` query on the counter version would
    // be misleading on restart.
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_uptime_seconds Adapter uptime in seconds since process boot."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_uptime_seconds gauge");
    let _ = writeln!(out, "cosmon_adapter_uptime_seconds {uptime}");

    // Build / posture info. Two label-only gauges with value 1 — the
    // Prometheus idiom for surfacing static labels (cf.
    // `prometheus_build_info`).
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_build_info Static build metadata (version, posture)."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_build_info gauge");
    let _ = writeln!(
        out,
        "cosmon_adapter_build_info{{version=\"{}\",posture=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION"),
        posture
    );

    // HTTP responses by status class. A counter family with one label
    // (`class`) of bounded cardinality (5 values).
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_http_responses_total HTTP response count by status class."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_http_responses_total counter");
    let _ = writeln!(
        out,
        "cosmon_adapter_http_responses_total{{class=\"1xx\"}} {}",
        statuses.responses_1xx
    );
    let _ = writeln!(
        out,
        "cosmon_adapter_http_responses_total{{class=\"2xx\"}} {}",
        statuses.responses_2xx
    );
    let _ = writeln!(
        out,
        "cosmon_adapter_http_responses_total{{class=\"3xx\"}} {}",
        statuses.responses_3xx
    );
    let _ = writeln!(
        out,
        "cosmon_adapter_http_responses_total{{class=\"4xx\"}} {}",
        statuses.responses_4xx
    );
    let _ = writeln!(
        out,
        "cosmon_adapter_http_responses_total{{class=\"5xx\"}} {}",
        statuses.responses_5xx
    );

    // Admission rejects — labelled by stable reason string from
    // `RppRejectReason::label`. The label set is closed (≤ 25 known
    // labels, see error.rs), so cardinality stays bounded.
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_admission_rejects_total Admission-stage rejects by reason."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_admission_rejects_total counter");
    if rejects.is_empty() {
        // Always emit a zero-sample line so an empty registry still
        // produces a parseable family header (Prometheus convention).
        let _ = writeln!(
            out,
            "cosmon_adapter_admission_rejects_total{{reason=\"none\"}} 0"
        );
    } else {
        for (reason, count) in rejects {
            let _ = writeln!(
                out,
                "cosmon_adapter_admission_rejects_total{{reason=\"{}\"}} {}",
                escape_label(reason),
                count
            );
        }
    }

    // Rate-limiter configuration (static gauges). Useful so dashboards
    // can show "consumed/N" without out-of-band knowledge of the
    // operator's bucket size.
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_rate_limit_capacity Per-sub leaky-bucket burst capacity (tokens)."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_rate_limit_capacity gauge");
    let _ = writeln!(
        out,
        "cosmon_adapter_rate_limit_capacity {}",
        state.rate_limiter.capacity()
    );
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_rate_limit_leak_per_minute Per-sub leaky-bucket drain rate (tokens/min)."
    );
    let _ = writeln!(
        out,
        "# TYPE cosmon_adapter_rate_limit_leak_per_minute gauge"
    );
    let _ = writeln!(
        out,
        "cosmon_adapter_rate_limit_leak_per_minute {}",
        state.rate_limiter.leak_per_minute()
    );

    // Backend health — one sample per known backend. Status encoded
    // as a small integer so dashboards can render heatmaps without
    // string-matching:
    //   0 = configured-but-unused
    //   1 = down
    //   2 = degraded
    //   3 = ok
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_backend_status LLM backend rolling status (3=ok, 2=degraded, 1=down, 0=configured-but-unused)."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_backend_status gauge");
    if backends.is_empty() {
        let _ = writeln!(out, "cosmon_adapter_backend_status{{backend=\"none\"}} 0");
    } else {
        for b in &backends {
            let v: u8 = match b.status {
                crate::BackendStatus::Ok => 3,
                crate::BackendStatus::Degraded => 2,
                crate::BackendStatus::Down => 1,
                crate::BackendStatus::ConfiguredButUnused => 0,
            };
            let _ = writeln!(
                out,
                "cosmon_adapter_backend_status{{backend=\"{}\"}} {v}",
                escape_label(&b.name)
            );
        }
    }
    // p95 latency where available (only emitted when the backend has
    // ≥5 successful samples — see BackendHealthRegistry).
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_backend_latency_p95_ms 95th-percentile call latency over the recent window."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_backend_latency_p95_ms gauge");
    for b in &backends {
        if let Some(p95) = b.latency_p95_ms {
            let _ = writeln!(
                out,
                "cosmon_adapter_backend_latency_p95_ms{{backend=\"{}\"}} {p95}",
                escape_label(&b.name)
            );
        }
    }

    // JWKS — one sample per pinned issuer. Cardinality bounded by the
    // operator's JWKS set (typically a handful).
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_jwks_keys_loaded JWKS keys loaded per pinned issuer."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_jwks_keys_loaded gauge");
    if jwks_per_iss.is_empty() {
        let _ = writeln!(out, "cosmon_adapter_jwks_keys_loaded{{issuer=\"none\"}} 0");
    } else {
        for (iss, count) in &jwks_per_iss {
            let _ = writeln!(
                out,
                "cosmon_adapter_jwks_keys_loaded{{issuer=\"{}\"}} {count}",
                escape_label(iss)
            );
        }
    }

    // Nucleon map projections.
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_nucleon_bindings Active `(iss, sub)` bindings in the sealed nucleon map."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_nucleon_bindings gauge");
    let _ = writeln!(out, "cosmon_adapter_nucleon_bindings {bindings}");

    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_nucleon_noyaux Distinct noyaux covered by the nucleon map."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_nucleon_noyaux gauge");
    let _ = writeln!(out, "cosmon_adapter_nucleon_noyaux {noyaux}");

    // SSE bus subscribers — proxy for "active streaming clients".
    let _ = writeln!(
        out,
        "# HELP cosmon_adapter_events_subscribers Current subscribers to the SSE event bus."
    );
    let _ = writeln!(out, "# TYPE cosmon_adapter_events_subscribers gauge");
    let _ = writeln!(out, "cosmon_adapter_events_subscribers {subscribers}");

    out
}

/// Escape a Prometheus label value per the exposition format §3.1:
/// `\` → `\\`, `"` → `\"`, newline → `\n`. The label set we emit is
/// closed (status class, backend name, issuer URL, reason label), but
/// issuer URLs may carry special characters — we escape defensively.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// `/diagnostics` JSON body. Wire-stable shape: a JSON object with a
/// flat top-level layout (each known projection has its own key) so a
/// dashboard can render fields without walking unknown structures.
#[derive(Debug, Serialize)]
struct DiagnosticsBody {
    service: &'static str,
    version: &'static str,
    posture: &'static str,
    uptime_seconds: u64,
    nucleon_map: HabilitationMapDiag,
    jwks: JwksDiag,
    backends: BackendsDiag,
    rate_limit: RateLimitDiag,
    events: EventsDiag,
    rejects: RejectsDiag,
    http_responses: crate::StatusClassCounts,
}

#[derive(Debug, Serialize)]
struct HabilitationMapDiag {
    bindings: usize,
    noyaux: usize,
}

#[derive(Debug, Serialize)]
struct JwksDiag {
    issuer_count: usize,
    total_keys: usize,
    /// Per-issuer key count. Sorted by issuer for stable diffing.
    by_issuer: Vec<JwksIssuerDiag>,
}

#[derive(Debug, Serialize)]
struct JwksIssuerDiag {
    issuer: String,
    keys: usize,
}

#[derive(Debug, Serialize)]
struct BackendsDiag {
    count: usize,
    /// Recopie of the `/health/backends` snapshot so a single
    /// `/diagnostics` round-trip covers the dashboard.
    snapshot: Vec<crate::BackendHealth>,
}

#[derive(Debug, Serialize)]
struct RateLimitDiag {
    capacity: f64,
    leak_per_minute: f64,
    leak_per_hour: f64,
}

#[derive(Debug, Serialize)]
struct EventsDiag {
    subscribers: usize,
    capacity: usize,
}

#[derive(Debug, Serialize)]
struct RejectsDiag {
    total: u64,
    by_reason: Vec<RejectReasonDiag>,
}

#[derive(Debug, Serialize)]
struct RejectReasonDiag {
    reason: &'static str,
    count: u64,
}

/// `GET /diagnostics` — JSON diagnostic snapshot.
///
/// Wire format is stable but additive: new keys may appear in later
/// versions; clients must not reject unknown fields. The route is
/// operational (no JWT gate, outside `/v1/`).
pub async fn diagnostics_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let posture = match state.posture {
        crate::Posture::Active => "active",
        crate::Posture::Prepared => "prepared",
    };
    let jwks_per_iss = state.jwks.load().key_counts_by_issuer();
    let total_keys: usize = jwks_per_iss.iter().map(|(_, n)| *n).sum();
    let backends_snapshot = state.backend_health.snapshot();
    let rejects = state.metrics.rejects_snapshot();
    let nm = state.nucleon_map.load();
    let body = DiagnosticsBody {
        service: "cosmon-rpp-adapter",
        version: env!("CARGO_PKG_VERSION"),
        posture,
        uptime_seconds: state.metrics.uptime_seconds(),
        nucleon_map: HabilitationMapDiag {
            bindings: nm.binding_count(),
            noyaux: nm.noyaux().len(),
        },
        jwks: JwksDiag {
            issuer_count: jwks_per_iss.len(),
            total_keys,
            by_issuer: jwks_per_iss
                .into_iter()
                .map(|(issuer, keys)| JwksIssuerDiag { issuer, keys })
                .collect(),
        },
        backends: BackendsDiag {
            count: backends_snapshot.len(),
            snapshot: backends_snapshot,
        },
        rate_limit: RateLimitDiag {
            capacity: state.rate_limiter.capacity(),
            leak_per_minute: state.rate_limiter.leak_per_minute(),
            leak_per_hour: state.rate_limiter.leak_per_hour(),
        },
        events: EventsDiag {
            subscribers: state.events.receiver_count(),
            capacity: crate::events_bus::DEFAULT_CAPACITY,
        },
        rejects: RejectsDiag {
            total: state.metrics.rejects_total(),
            by_reason: rejects
                .into_iter()
                .map(|(reason, count)| RejectReasonDiag { reason, count })
                .collect(),
        },
        http_responses: state.metrics.status_class_counts(),
    };
    Json(serde_json::to_value(&body).unwrap_or_else(|_| json!({"error": "diagnostics_render"})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_label_quotes_and_backslash() {
        assert_eq!(escape_label("plain"), "plain");
        assert_eq!(escape_label("with \"quote\""), "with \\\"quote\\\"");
        assert_eq!(escape_label("back\\slash"), "back\\\\slash");
        assert_eq!(escape_label("multi\nline"), "multi\\nline");
    }
}
