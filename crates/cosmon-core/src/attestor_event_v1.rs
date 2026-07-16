// SPDX-License-Identifier: AGPL-3.0-only

//! `AttestorEventV1` — the typed NDJSON ledger consumed by the noogram
//! spec auditors (`MycelialGate`, `AttestorGraph`, `WitnessFreshness`).
//!
//! These are the events the upstream noogram pipeline writes to
//! `.cosmon/state/attestor-events.jsonl`. The audit binary replays
//! them through one of the registered spec auditors via
//! `cs spec-audit --spec <name> --events <path>`.
//!
//! The schema is **versioned** at `v1` and published as JSON Schema at
//! `cosmon/docs/specs/attestor-events.schema.json`. Adding a variant is
//! a non-breaking change as long as readers tolerate unknown `kind`
//! values; the auditors below ignore variants they do not understand
//! (the standard `UnknownVariant` shape).
//!
//! ## Why a typed ledger
//!
//! Every new TLA+ spec without a binding ledger is decoration at runtime:
//! a model that nothing at runtime is held against proves nothing. This
//! module is one half of the bind — the typed event the spec is checked
//! against; the other half is [`crate::attestor_audit`] (the per-spec
//! replay drivers).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Envelope — a single NDJSON row
// ---------------------------------------------------------------------------

/// One row of the `attestor-events.jsonl` ledger.
///
/// `seq` is a monotonically increasing sequence assigned at write
/// time; the auditors use it to attach drifts to the offending row.
/// `event` is the typed payload — see [`AttestorEventV1`] for the
/// variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttestorEnvelope {
    /// Schema version. Always `1` today; readers must reject other
    /// values (forward-incompatible change).
    #[serde(default = "default_version")]
    pub v: u32,
    /// Monotonic per-ledger sequence number.
    pub seq: u64,
    /// Wall-clock time at which the event was written (RFC 3339 UTC).
    pub ts: DateTime<Utc>,
    /// The typed payload.
    #[serde(flatten)]
    pub event: AttestorEventV1,
}

fn default_version() -> u32 {
    1
}

impl AttestorEnvelope {
    /// Construct an envelope with the current schema version (`v = 1`).
    #[must_use]
    pub fn new(seq: u64, ts: DateTime<Utc>, event: AttestorEventV1) -> Self {
        Self {
            v: 1,
            seq,
            ts,
            event,
        }
    }
}

// ---------------------------------------------------------------------------
// AttestorEventV1 — the typed payload
// ---------------------------------------------------------------------------

/// A typed event in the noogram attestor ledger.
///
/// `kind` is the discriminator on the wire. Every variant carries its
/// own `t` (the event's logical time, which may differ from the
/// envelope `ts` — e.g. a backfilled `ClusterMetadataEvent` snapshot
/// inserted after the fact).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttestorEventV1 {
    /// An attestor was enrolled into the graph and is eligible to
    /// witness from `t` onwards (subject to other invariants).
    AttestorEnrol {
        /// Attestor that was enrolled.
        attestor: AttestorId,
        /// Logical time of the enrolment.
        t: DateTime<Utc>,
    },
    /// An attestor's enrolment expired at `t`. Witnessing past this
    /// time is a drift surfaced by the `AttestorGraph` and
    /// `WitnessFreshness` auditors.
    AttestorExpired {
        /// Attestor whose enrolment expired.
        attestor: AttestorId,
        /// Logical time of the expiry.
        t: DateTime<Utc>,
    },
    /// A snapshot of the attestor's institutional cluster metadata at
    /// time `t`. The metadata is intentionally a *snapshot* and not a
    /// permanent attribute — an attestor who moves institutions writes
    /// a new snapshot. The `WitnessFreshness` auditor checks that the
    /// most recent snapshot before an absorption is within the
    /// freshness window.
    ClusterMetadata {
        /// Attestor the snapshot describes.
        attestor: AttestorId,
        /// Logical time of the snapshot.
        t: DateTime<Utc>,
        /// Institution name at time `t`.
        institution: String,
        /// Jurisdiction (ISO country code or freeform region).
        jurisdiction: String,
        /// Optional funder name at time `t`.
        funder: Option<String>,
    },
    /// A paper was absorbed at `t` with the listed witnesses. The
    /// `MycelialGate` and `AttestorGraph` auditors check the witness
    /// set against the spec.
    Absorption {
        /// Stable id of the absorption (e.g. `abs-2026-05-16-001`).
        absorption_id: AbsorptionId,
        /// Attestor who absorbed the paper.
        attestor: AttestorId,
        /// Subject-matter vertical (free-form tag).
        vertical: String,
        /// Logical time of the absorption.
        t: DateTime<Utc>,
        /// Witnesses that gated the absorption (order is not
        /// significant; multiplicity is a drift).
        witnesses: Vec<AttestorId>,
        /// URI of the public artefact whose absorption is recorded.
        public_artefact_uri: String,
    },
    /// An audit outcome for a prior absorption: `ok`, `entryist`, or
    /// `pending`. The auditors consume this for cross-reference
    /// integrity (every `Absorption` referenced by `AuditOutcome` must
    /// exist).
    AuditOutcome {
        /// Absorption whose audit outcome is recorded.
        absorption_id: AbsorptionId,
        /// Outcome verdict.
        outcome: AuditOutcome,
        /// Logical time of the audit outcome.
        t: DateTime<Utc>,
    },
}

impl AttestorEventV1 {
    /// The logical event time `t` — distinct from the envelope's
    /// write-time `ts`.
    #[must_use]
    pub fn t(&self) -> DateTime<Utc> {
        match self {
            Self::AttestorEnrol { t, .. }
            | Self::AttestorExpired { t, .. }
            | Self::ClusterMetadata { t, .. }
            | Self::Absorption { t, .. }
            | Self::AuditOutcome { t, .. } => *t,
        }
    }
}

/// Outcome of an audit on an absorption.
///
/// `entryist` means the audit found the absorption violated the
/// mycelial-gate invariant (the witnesses did not reflect diverse,
/// independent attestation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The absorption is clean — no entryism witnessed.
    Ok,
    /// The audit found entryism — the absorption violated the
    /// mycelial-gate invariant (witnesses were not diverse enough).
    Entryist,
    /// The audit is still pending; no verdict yet.
    Pending,
}

// ---------------------------------------------------------------------------
// Newtype identifiers — keep the payload self-documenting
// ---------------------------------------------------------------------------

/// Opaque attestor identifier (e.g. an ORCID-like handle).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttestorId(
    /// Raw string identifier.
    pub String,
);

impl AttestorId {
    /// Construct an [`AttestorId`] from anything string-like.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque absorption identifier (e.g. `abs-2026-05-16-001`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AbsorptionId(
    /// Raw string identifier.
    pub String,
);

impl AbsorptionId {
    /// Construct an [`AbsorptionId`] from anything string-like.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// NDJSON parser (pure — zero I/O)
// ---------------------------------------------------------------------------

/// Failure parsing an `attestor-events.jsonl` line into an
/// [`AttestorEnvelope`].
///
/// This error is **pure**: it is produced by [`parse_line`] / [`parse_all`]
/// from string content the *caller* already read. The filesystem seam lives
/// in the adapter (`cosmon_state::attestor_log::read_all`), not in this
/// domain crate — keeping `cosmon-core` zero-I/O (INV-DOMAIN-PURE-NO-IO,
/// ADR-082). The `line` field is the 1-based source line for diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum AttestorParseError {
    /// A line was not valid `AttestorEnvelope` JSON.
    #[error("line {line}: {source}")]
    Json {
        /// 1-based line number of the offending row.
        line: usize,
        /// The underlying serde error.
        source: serde_json::Error,
    },
    /// A line carried a schema version other than `1` (forward-incompatible).
    #[error("line {line}: unsupported schema version v={version}")]
    UnsupportedVersion {
        /// 1-based line number of the offending row.
        line: usize,
        /// The unsupported version encountered.
        version: u32,
    },
}

/// Parse a single NDJSON line into an envelope (pure).
///
/// Returns `Ok(None)` for a blank/whitespace-only line (those are skipped
/// during replay). `line` is the 1-based source index, used only in error
/// messages.
///
/// # Errors
///
/// Returns [`AttestorParseError::Json`] on malformed JSON, or
/// [`AttestorParseError::UnsupportedVersion`] when the row's `v` is not `1`.
pub fn parse_line(line: usize, raw: &str) -> Result<Option<AttestorEnvelope>, AttestorParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let env: AttestorEnvelope = serde_json::from_str(trimmed)
        .map_err(|source| AttestorParseError::Json { line, source })?;
    if env.v != 1 {
        return Err(AttestorParseError::UnsupportedVersion {
            line,
            version: env.v,
        });
    }
    Ok(Some(env))
}

/// Parse the full contents of an `attestor-events.jsonl` document (pure).
///
/// Empty lines are skipped; the first malformed or wrong-version line fails
/// the whole parse (the audit is not a recovery tool). The string is read by
/// an adapter — see `cosmon_state::attestor_log::read_all`.
///
/// # Errors
///
/// Propagates the first [`AttestorParseError`] from [`parse_line`].
pub fn parse_all(content: &str) -> Result<Vec<AttestorEnvelope>, AttestorParseError> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if let Some(env) = parse_line(i + 1, line)? {
            out.push(env);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: &str) -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn enrol_roundtrips_through_json() {
        let env = AttestorEnvelope::new(
            0,
            t("2026-05-16T12:00:00Z"),
            AttestorEventV1::AttestorEnrol {
                attestor: AttestorId::new("a1"),
                t: t("2026-05-16T12:00:00Z"),
            },
        );
        let j = serde_json::to_string(&env).unwrap();
        assert!(j.contains("\"kind\":\"attestor_enrol\""));
        let back: AttestorEnvelope = serde_json::from_str(&j).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn absorption_with_witnesses_roundtrips() {
        let env = AttestorEnvelope::new(
            3,
            t("2026-05-16T15:00:00Z"),
            AttestorEventV1::Absorption {
                absorption_id: AbsorptionId::new("abs-001"),
                attestor: AttestorId::new("a1"),
                vertical: "physics".into(),
                t: t("2026-05-16T15:00:00Z"),
                witnesses: vec![AttestorId::new("a2"), AttestorId::new("a3")],
                public_artefact_uri: "https://example.org/p1".into(),
            },
        );
        let j = serde_json::to_string(&env).unwrap();
        let back: AttestorEnvelope = serde_json::from_str(&j).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn audit_outcome_serialises_snake_case() {
        let env = AttestorEnvelope::new(
            4,
            Utc.with_ymd_and_hms(2026, 5, 16, 16, 0, 0).unwrap(),
            AttestorEventV1::AuditOutcome {
                absorption_id: AbsorptionId::new("abs-001"),
                outcome: AuditOutcome::Entryist,
                t: Utc.with_ymd_and_hms(2026, 5, 16, 16, 0, 0).unwrap(),
            },
        );
        let j = serde_json::to_string(&env).unwrap();
        assert!(j.contains("\"outcome\":\"entryist\""));
    }

    #[test]
    fn version_default_is_one() {
        let j = r#"{"seq":0,"ts":"2026-05-16T12:00:00Z","kind":"attestor_enrol","attestor":"a1","t":"2026-05-16T12:00:00Z"}"#;
        let env: AttestorEnvelope = serde_json::from_str(j).unwrap();
        assert_eq!(env.v, 1);
    }

    #[test]
    fn parse_all_rejects_unknown_version() {
        let doc = "{\"v\":2,\"seq\":0,\"ts\":\"2026-05-16T12:00:00Z\",\"kind\":\"attestor_enrol\",\"attestor\":\"a1\",\"t\":\"2026-05-16T12:00:00Z\"}\n";
        let err = parse_all(doc).unwrap_err();
        assert!(matches!(
            err,
            AttestorParseError::UnsupportedVersion { version: 2, .. }
        ));
    }

    #[test]
    fn parse_all_skips_blank_lines_and_roundtrips() {
        let env = AttestorEnvelope::new(
            0,
            t("2026-05-16T12:00:00Z"),
            AttestorEventV1::AttestorEnrol {
                attestor: AttestorId::new("a1"),
                t: t("2026-05-16T12:00:00Z"),
            },
        );
        let line = serde_json::to_string(&env).unwrap();
        let doc = format!("\n{line}\n\n");
        let parsed = parse_all(&doc).unwrap();
        assert_eq!(parsed, vec![env]);
    }

    #[test]
    fn parse_line_returns_none_for_blank() {
        assert_eq!(parse_line(1, "   ").unwrap(), None);
    }
}
