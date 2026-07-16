// SPDX-License-Identifier: AGPL-3.0-only

//! Authz-decision instrumentation — IFBDD émission de faits at the
//! `cosmon_state::ops` boundary.
//!
//! T-AUTHZ-INSTR sibling of T1's `EngineCallEntered` (which lives in
//! `cosmon-api`). Records every authz decision evaluated at the
//! `ops::<verb>` boundary so the operator can later answer two empirical
//! questions:
//!
//! 1. Per verb, which `(subject_kind, scope)` pair is invoked, and what
//!    is the latency of the V0 trivial check? The pattern drives the
//!    future scope-by-verb RBAC grid (T-RPP-V0).
//! 2. Which subjects show up in practice (`operator` vs `jwt:<sub>`)?
//!    The distribution informs the policy that gets crystallised later.
//!
//! Strict IFBDD discipline: we collect the patterns *before* hardening
//! the grid. A grid posed before the data is a grid posed against
//! imagined traffic.
//!
//! # Observation, not enforcement
//!
//! The instrumentation never blocks the hot path, never persists into
//! `state.json` (this is system telemetry, not a domain event), and any
//! IO failure is silently swallowed. The seal-pattern is the same one
//! used by [`crate::briefing_seal`]: trace, not lock.
//!
//! # Sink
//!
//! One NDJSON sink: `{state_dir}/instrumentation/authz.jsonl`. The
//! `COSMON_AUTHZ_INSTRUMENTATION_PATH` environment variable overrides
//! the path — used by integration tests that point it at a tempfile.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// V0 authz decision computed at the verb boundary.
///
/// - `Allow` — explicit grant. The CLI operator subject yields this in
///   V0 (implicit, full-trust grant).
/// - `Deny` — explicit reject. Reserved for the future scope grid;
///   never emitted by the V0 trivial-check.
/// - `Absent` — no rule applies yet. JWT subjects yield this until a
///   scope-by-verb grid lands (T-RPP-V0). The instrumentation captures
///   the pattern so we can decide *what* the grid should be.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthzDecision {
    /// V0 grant for the trusted operator subject.
    Allow,
    /// Explicit reject — reserved for the future grid.
    Deny,
    /// No rule applies yet — V0 default for JWT subjects.
    Absent,
}

/// One recorded authz-decision event.
///
/// Wire-format-stable fields: external scrapers may rely on the JSON
/// shape across V0..V1. New optional fields use
/// `#[serde(skip_serializing_if = "Option::is_none")]` so legacy readers
/// keep working.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthzDecisionEvaluated {
    /// The cosmon verb invoked (e.g. `observe`, `tag`, `nucleate`).
    pub verb: String,
    /// Stringly-typed identification of the subject. V0 vocabulary:
    /// `"operator"`, `"jwt:<sub>"`, `"absent"`. Refactor to a typed
    /// `Subject` once T-SUBJECT lands.
    pub subject_kind: String,
    /// The scope that the V0 grid would require for this verb. `None`
    /// while the grid is not figée — emitted as omitted JSON field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_required: Option<String>,
    /// Decision yielded by the V0 trivial check.
    pub decision: AuthzDecision,
    /// Source of the scope grant when `decision = Allow`. V0 vocabulary:
    /// `"jwt"` (token carried the scope directly) or `"binding"` (the
    /// admin nucleon binding granted the scope implicitly, e.g. an
    /// you sandbox binding that materialises a scope the upstream
    /// `IdP` — Forgejo — cannot issue). Omitted JSON field when absent
    /// (turing-style wire stability: legacy readers keep parsing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_source: Option<String>,
    /// Wall-clock latency of the V0 check itself, in milliseconds.
    pub latency_ms: u64,
    /// UTC timestamp when the event was emitted.
    pub timestamp: DateTime<Utc>,
}

/// Relative NDJSON path under the cosmon state directory.
pub const AUTHZ_NDJSON_RELATIVE_PATH: &str = "instrumentation/authz.jsonl";

/// Resolve the absolute NDJSON path. The
/// `COSMON_AUTHZ_INSTRUMENTATION_PATH` env var wins over the default
/// `{state_dir}/instrumentation/authz.jsonl` so integration tests can
/// isolate captures without sharing the tempdir-aware ops API surface.
#[must_use]
pub fn resolve_authz_path(state_dir: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("COSMON_AUTHZ_INSTRUMENTATION_PATH") {
        return PathBuf::from(p);
    }
    state_dir.join(AUTHZ_NDJSON_RELATIVE_PATH)
}

/// Process-wide append lock — keeps two concurrent threads from
/// interleaving partial JSON lines on the same NDJSON file.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Emit one [`AuthzDecisionEvaluated`] event to the NDJSON sink.
///
/// Best-effort: the function never panics, never blocks the caller on
/// an I/O error, and never reports a result. A serialise or write
/// failure is silently swallowed in keeping with the seal pattern from
/// [`crate::briefing_seal`].
///
/// # V0 vocabulary
///
/// - `subject_kind = "operator"` for the trusted CLI subject.
/// - `subject_kind = "jwt:<sub>"` once T-RPP-V0 wires JWT-bearing
///   callers; the subject `<sub>` claim is interpolated into the value.
/// - `scope_required = None` while the grid is not figée. Emitted as
///   omitted JSON field so the wire shape stays stable.
pub fn emit_authz_decision(
    state_dir: &Path,
    verb: &str,
    subject_kind: &str,
    scope_required: Option<&str>,
    decision: AuthzDecision,
    latency_ms: u64,
) {
    emit_authz_decision_with_source(
        state_dir,
        verb,
        subject_kind,
        scope_required,
        decision,
        None,
        latency_ms,
    );
}

/// Same as [`emit_authz_decision`] plus a `grant_source` discriminator.
///
/// Use this variant when the scope check consulted more than one
/// source. Admin-nucleon bindings can grant scopes; routes that consult
/// them call this function with `grant_source =
/// Some("jwt"|"binding")` so the audit trail records *which* gate
/// produced the Allow. The trivial wrapper [`emit_authz_decision`]
/// is retained for the cosmon-state ops layer, which has no
/// binding-source notion.
pub fn emit_authz_decision_with_source(
    state_dir: &Path,
    verb: &str,
    subject_kind: &str,
    scope_required: Option<&str>,
    decision: AuthzDecision,
    grant_source: Option<&str>,
    latency_ms: u64,
) {
    let event = AuthzDecisionEvaluated {
        verb: verb.to_owned(),
        subject_kind: subject_kind.to_owned(),
        scope_required: scope_required.map(str::to_owned),
        decision,
        grant_source: grant_source.map(str::to_owned),
        latency_ms,
        timestamp: Utc::now(),
    };

    let path = resolve_authz_path(state_dir);
    let Ok(line) = serde_json::to_string(&event) else {
        return;
    };

    let _guard = FILE_LOCK.lock().ok();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
}

/// Read every event from an NDJSON file. Used by integration tests and
/// by the empirical mini-rapport tooling. Returns an empty `Vec` when
/// the file does not exist.
///
/// Malformed lines are silently skipped — the V0 instrumentation is a
/// best-effort capture, not a contract; a partial line from a crash
/// must not poison the read path.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the file exists but
/// cannot be read (e.g. permissions).
pub fn read_authz_ndjson(path: &Path) -> std::io::Result<Vec<AuthzDecisionEvaluated>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<AuthzDecisionEvaluated>(trimmed) {
            out.push(ev);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn emit_creates_ndjson_under_state_dir() {
        let tmp = TempDir::new().unwrap();
        // Make sure the env var override is not interfering with the
        // default-path test.
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");
        emit_authz_decision(
            tmp.path(),
            "observe",
            "operator",
            None,
            AuthzDecision::Allow,
            0,
        );
        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        assert!(path.exists(), "ndjson should be created at {path:?}");
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verb, "observe");
        assert_eq!(events[0].subject_kind, "operator");
        assert!(events[0].scope_required.is_none());
        assert_eq!(events[0].decision, AuthzDecision::Allow);
    }

    #[test]
    fn emit_appends_multiple_events() {
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");
        emit_authz_decision(
            tmp.path(),
            "observe",
            "operator",
            None,
            AuthzDecision::Allow,
            0,
        );
        emit_authz_decision(
            tmp.path(),
            "observe",
            "jwt:tenant_auditor",
            None,
            AuthzDecision::Absent,
            1,
        );
        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].subject_kind, "jwt:tenant_auditor");
        assert_eq!(events[1].decision, AuthzDecision::Absent);
    }

    #[test]
    fn scope_required_is_omitted_when_none() {
        // Wire format must stay stable across V0/V1: the omit-when-none
        // discipline is what lets a future grid add the field without
        // breaking external scrapers.
        let event = AuthzDecisionEvaluated {
            verb: "observe".into(),
            subject_kind: "operator".into(),
            scope_required: None,
            decision: AuthzDecision::Allow,
            grant_source: None,
            latency_ms: 0,
            timestamp: Utc::now(),
        };
        let s = serde_json::to_string(&event).unwrap();
        assert!(
            !s.contains("scope_required"),
            "scope_required must be omitted when None: {s}"
        );
        assert!(
            !s.contains("grant_source"),
            "grant_source must be omitted when None: {s}"
        );
    }

    #[test]
    fn read_returns_empty_when_missing() {
        let path = std::path::PathBuf::from("/tmp/cosmon-authz-does-not-exist.jsonl");
        let events = read_authz_ndjson(&path).unwrap();
        assert!(events.is_empty());
    }
}
