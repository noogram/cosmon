// SPDX-License-Identifier: AGPL-3.0-only

//! Noogram spec auditors — replay [`crate::attestor_event_v1`] ledgers
//! through a per-spec invariant checker and emit
//! [`crate::audit::Drift::SpecInvariantViolation`] findings.
//!
//! Three auditors live here:
//!
//! * `mycelial_gate` — the witness diversity gate on every
//!   [`AttestorEventV1::Absorption`].
//! * `attestor_graph` — temporal sanity on the attestor lifecycle
//!   (enrol before witnessing, no witnessing past expiry, cluster
//!   metadata exists at witness time).
//! * `witness_freshness` — most-recent `ClusterMetadata` snapshot
//!   before each absorption must be within the freshness window.
//!
//! Each auditor is a pure function over `&[AttestorEnvelope]`. They do
//! not perform I/O; the CLI is the I/O boundary.
//!
//! ## Why these auditors exist
//!
//! A specification that promises *"an external reviewer can audit
//! (specification, kernel binary, …)"* is hollow unless something actually
//! replays the ledger and checks the invariants. The three auditors below
//! are that something: they give the noogram specs a binding, machine-checked
//! ledger so the audit promise is not vacuous after publication.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};

use crate::attestor_event_v1::{AttestorEnvelope, AttestorEventV1, AttestorId};
use crate::audit::{AuditReport, Drift};
use crate::event_v2::Seq;

// ---------------------------------------------------------------------------
// MycelialGate
// ---------------------------------------------------------------------------

/// Minimum number of distinct witnesses required on every absorption.
///
/// The mycelial-gate invariant in the noogram spec is *witness
/// diversity*: an absorption with fewer than `MIN_WITNESSES` distinct
/// attestors is not gated. The threshold is conservative — the spec
/// itself can tighten it; this module is the audit floor.
pub const MIN_WITNESSES: usize = 2;

/// Replay through the `MycelialGate` auditor.
///
/// Drifts emitted:
///
/// * `insufficient_witnesses` — fewer than [`MIN_WITNESSES`] distinct
///   attestors on an absorption.
/// * `self_witness` — the absorbing attestor appears in its own
///   witness list (a degenerate gate).
/// * `duplicate_witness` — the witness list contains the same
///   attestor twice (multiplicity does not buy diversity).
#[must_use]
pub fn audit_mycelial_gate(envelopes: &[AttestorEnvelope]) -> AuditReport {
    let mut drifts = Vec::new();
    let mut absorptions: u64 = 0;

    for env in envelopes {
        let AttestorEventV1::Absorption {
            absorption_id,
            attestor,
            witnesses,
            ..
        } = &env.event
        else {
            continue;
        };
        absorptions += 1;

        let mut seen: HashSet<&AttestorId> = HashSet::new();
        let mut had_self = false;
        let mut had_dup = false;
        for w in witnesses {
            if w == attestor {
                had_self = true;
            }
            if !seen.insert(w) {
                had_dup = true;
            }
        }
        if had_self {
            drifts.push(Drift::SpecInvariantViolation {
                seq: Seq(env.seq),
                spec: "mycelial-gate".to_owned(),
                invariant: "self_witness".to_owned(),
                subject: Some(absorption_id.as_str().to_owned()),
                note: format!(
                    "absorption {} lists {} (the absorbing attestor) as a witness",
                    absorption_id.as_str(),
                    attestor.as_str()
                ),
            });
        }
        if had_dup {
            drifts.push(Drift::SpecInvariantViolation {
                seq: Seq(env.seq),
                spec: "mycelial-gate".to_owned(),
                invariant: "duplicate_witness".to_owned(),
                subject: Some(absorption_id.as_str().to_owned()),
                note: format!(
                    "absorption {} witness list has duplicates",
                    absorption_id.as_str()
                ),
            });
        }
        // Count distinct *non-self* witnesses for the gate threshold.
        let distinct_nonself = seen.iter().filter(|w| **w != attestor).count();
        if distinct_nonself < MIN_WITNESSES {
            drifts.push(Drift::SpecInvariantViolation {
                seq: Seq(env.seq),
                spec: "mycelial-gate".to_owned(),
                invariant: "insufficient_witnesses".to_owned(),
                subject: Some(absorption_id.as_str().to_owned()),
                note: format!(
                    "absorption {} has {} distinct non-self witnesses (min: {})",
                    absorption_id.as_str(),
                    distinct_nonself,
                    MIN_WITNESSES
                ),
            });
        }
    }
    drifts.sort_by_key(Drift::seq);
    AuditReport {
        drifts,
        events_replayed: envelopes.len() as u64,
        molecules_seen: absorptions, // re-purposed: number of absorptions audited
    }
}

// ---------------------------------------------------------------------------
// AttestorGraph
// ---------------------------------------------------------------------------

/// Replay through the `AttestorGraph` auditor.
///
/// Drifts emitted:
///
/// * `witness_not_enrolled` — a witness appears in an absorption
///   without a preceding `AttestorEnrol`.
/// * `witness_expired` — a witness appears in an absorption after its
///   `AttestorExpired` event.
/// * `cluster_metadata_missing` — a witness appears in an absorption
///   with no `ClusterMetadata` snapshot recorded at any prior `t`.
/// * `attestor_not_enrolled` — the absorbing attestor itself is not
///   enrolled.
/// * `audit_outcome_orphan` — an `AuditOutcome` references an
///   absorption that never appeared in the ledger.
#[must_use]
#[allow(clippy::too_many_lines, clippy::missing_panics_doc)]
pub fn audit_attestor_graph(envelopes: &[AttestorEnvelope]) -> AuditReport {
    let mut drifts = Vec::new();
    let mut enrolments: HashMap<AttestorId, DateTime<Utc>> = HashMap::new();
    let mut expirations: HashMap<AttestorId, DateTime<Utc>> = HashMap::new();
    let mut has_metadata: HashMap<AttestorId, DateTime<Utc>> = HashMap::new();
    let mut absorption_ids: HashSet<String> = HashSet::new();
    let mut attestors_seen: HashSet<AttestorId> = HashSet::new();

    for env in envelopes {
        match &env.event {
            AttestorEventV1::AttestorEnrol { attestor, t } => {
                enrolments.entry(attestor.clone()).or_insert(*t);
                attestors_seen.insert(attestor.clone());
            }
            AttestorEventV1::AttestorExpired { attestor, t } => {
                expirations.insert(attestor.clone(), *t);
                attestors_seen.insert(attestor.clone());
            }
            AttestorEventV1::ClusterMetadata { attestor, t, .. } => {
                let entry = has_metadata.entry(attestor.clone()).or_insert(*t);
                if *t < *entry {
                    *entry = *t;
                }
                attestors_seen.insert(attestor.clone());
            }
            AttestorEventV1::Absorption {
                absorption_id,
                attestor,
                t,
                witnesses,
                ..
            } => {
                absorption_ids.insert(absorption_id.as_str().to_owned());
                attestors_seen.insert(attestor.clone());

                // Absorbing attestor must itself be enrolled by t.
                let enrolled_by = enrolments.get(attestor).copied();
                if enrolled_by.is_none() || enrolled_by.unwrap() > *t {
                    drifts.push(Drift::SpecInvariantViolation {
                        seq: Seq(env.seq),
                        spec: "attestor-graph".to_owned(),
                        invariant: "attestor_not_enrolled".to_owned(),
                        subject: Some(attestor.as_str().to_owned()),
                        note: format!(
                            "absorption {} by {} but no enrol event ≤ t={}",
                            absorption_id.as_str(),
                            attestor.as_str(),
                            t.to_rfc3339()
                        ),
                    });
                }

                for w in witnesses {
                    attestors_seen.insert(w.clone());

                    let enrolled = enrolments.get(w).copied();
                    if enrolled.is_none() || enrolled.unwrap() > *t {
                        drifts.push(Drift::SpecInvariantViolation {
                            seq: Seq(env.seq),
                            spec: "attestor-graph".to_owned(),
                            invariant: "witness_not_enrolled".to_owned(),
                            subject: Some(w.as_str().to_owned()),
                            note: format!(
                                "witness {} not enrolled at absorption {} (t={})",
                                w.as_str(),
                                absorption_id.as_str(),
                                t.to_rfc3339()
                            ),
                        });
                    }
                    if let Some(exp_t) = expirations.get(w).copied() {
                        if exp_t <= *t {
                            drifts.push(Drift::SpecInvariantViolation {
                                seq: Seq(env.seq),
                                spec: "attestor-graph".to_owned(),
                                invariant: "witness_expired".to_owned(),
                                subject: Some(w.as_str().to_owned()),
                                note: format!(
                                    "witness {} expired at {} but used in absorption {} (t={})",
                                    w.as_str(),
                                    exp_t.to_rfc3339(),
                                    absorption_id.as_str(),
                                    t.to_rfc3339()
                                ),
                            });
                        }
                    }
                    let meta = has_metadata.get(w).copied();
                    if meta.is_none() || meta.unwrap() > *t {
                        drifts.push(Drift::SpecInvariantViolation {
                            seq: Seq(env.seq),
                            spec: "attestor-graph".to_owned(),
                            invariant: "cluster_metadata_missing".to_owned(),
                            subject: Some(w.as_str().to_owned()),
                            note: format!(
                                "witness {} has no cluster_metadata snapshot ≤ t={}",
                                w.as_str(),
                                t.to_rfc3339()
                            ),
                        });
                    }
                }
            }
            AttestorEventV1::AuditOutcome { absorption_id, .. } => {
                if !absorption_ids.contains(absorption_id.as_str()) {
                    drifts.push(Drift::SpecInvariantViolation {
                        seq: Seq(env.seq),
                        spec: "attestor-graph".to_owned(),
                        invariant: "audit_outcome_orphan".to_owned(),
                        subject: Some(absorption_id.as_str().to_owned()),
                        note: format!(
                            "audit_outcome references unknown absorption {}",
                            absorption_id.as_str()
                        ),
                    });
                }
            }
        }
    }

    drifts.sort_by_key(Drift::seq);
    AuditReport {
        drifts,
        events_replayed: envelopes.len() as u64,
        molecules_seen: attestors_seen.len() as u64,
    }
}

// ---------------------------------------------------------------------------
// WitnessFreshness
// ---------------------------------------------------------------------------

/// Default freshness window for cluster-metadata snapshots — 365 days.
///
/// A witness's most recent `ClusterMetadata` snapshot before an
/// absorption `t` must be within this window. The constant lives here
/// so callers (or future config) can override it.
pub const DEFAULT_FRESHNESS_DAYS: i64 = 365;

/// Replay through the `WitnessFreshness` auditor.
///
/// Drifts emitted:
///
/// * `stale_metadata` — a witness's most recent `ClusterMetadata`
///   snapshot before the absorption is older than the freshness
///   window.
/// * `no_metadata_in_window` — a witness has no `ClusterMetadata`
///   snapshot at all within the window (independent of whether one
///   exists outside it).
#[must_use]
pub fn audit_witness_freshness(envelopes: &[AttestorEnvelope]) -> AuditReport {
    audit_witness_freshness_with_window(envelopes, DEFAULT_FRESHNESS_DAYS)
}

/// `WitnessFreshness` audit with an explicit freshness window in
/// days. Exposed for tests and future config overrides.
#[must_use]
pub fn audit_witness_freshness_with_window(
    envelopes: &[AttestorEnvelope],
    window_days: i64,
) -> AuditReport {
    let window = Duration::days(window_days);
    let mut drifts = Vec::new();
    // attestor → all metadata snapshots in arrival order.
    let mut snapshots: HashMap<AttestorId, Vec<DateTime<Utc>>> = HashMap::new();
    let mut absorptions: u64 = 0;
    let mut attestors_seen: HashSet<AttestorId> = HashSet::new();

    for env in envelopes {
        match &env.event {
            AttestorEventV1::ClusterMetadata { attestor, t, .. } => {
                snapshots.entry(attestor.clone()).or_default().push(*t);
                attestors_seen.insert(attestor.clone());
            }
            AttestorEventV1::Absorption {
                absorption_id,
                t,
                witnesses,
                ..
            } => {
                absorptions += 1;
                for w in witnesses {
                    attestors_seen.insert(w.clone());
                    let snaps = snapshots.get(w);
                    let most_recent_before =
                        snaps.and_then(|v| v.iter().filter(|s| **s <= *t).max().copied());

                    match most_recent_before {
                        None => {
                            drifts.push(Drift::SpecInvariantViolation {
                                seq: Seq(env.seq),
                                spec: "witness-freshness".to_owned(),
                                invariant: "no_metadata_in_window".to_owned(),
                                subject: Some(w.as_str().to_owned()),
                                note: format!(
                                    "witness {} has no cluster_metadata snapshot ≤ t={} (absorption {})",
                                    w.as_str(),
                                    t.to_rfc3339(),
                                    absorption_id.as_str(),
                                ),
                            });
                        }
                        Some(snap_t) => {
                            if *t - snap_t > window {
                                drifts.push(Drift::SpecInvariantViolation {
                                    seq: Seq(env.seq),
                                    spec: "witness-freshness".to_owned(),
                                    invariant: "stale_metadata".to_owned(),
                                    subject: Some(w.as_str().to_owned()),
                                    note: format!(
                                        "witness {} cluster_metadata snapshot at {} is older than {} days at absorption {} (t={})",
                                        w.as_str(),
                                        snap_t.to_rfc3339(),
                                        window_days,
                                        absorption_id.as_str(),
                                        t.to_rfc3339()
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            AttestorEventV1::AttestorEnrol { attestor, .. }
            | AttestorEventV1::AttestorExpired { attestor, .. } => {
                attestors_seen.insert(attestor.clone());
            }
            AttestorEventV1::AuditOutcome { .. } => {}
        }
    }

    drifts.sort_by_key(Drift::seq);
    AuditReport {
        drifts,
        events_replayed: envelopes.len() as u64,
        molecules_seen: absorptions,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestor_event_v1::AbsorptionId;
    use chrono::TimeZone;

    fn t(year: i32, mo: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, mo, day, 12, 0, 0).unwrap()
    }
    fn aid(s: &str) -> AttestorId {
        AttestorId::new(s)
    }
    fn absid(s: &str) -> AbsorptionId {
        AbsorptionId::new(s)
    }
    fn env(seq: u64, ts: DateTime<Utc>, event: AttestorEventV1) -> AttestorEnvelope {
        AttestorEnvelope::new(seq, ts, event)
    }

    fn enrol(seq: u64, a: &str, ts: DateTime<Utc>) -> AttestorEnvelope {
        env(
            seq,
            ts,
            AttestorEventV1::AttestorEnrol {
                attestor: aid(a),
                t: ts,
            },
        )
    }
    fn cluster(seq: u64, a: &str, ts: DateTime<Utc>) -> AttestorEnvelope {
        env(
            seq,
            ts,
            AttestorEventV1::ClusterMetadata {
                attestor: aid(a),
                t: ts,
                institution: "Tenant-Demo".into(),
                jurisdiction: "EU".into(),
                funder: None,
            },
        )
    }
    fn absorb(
        seq: u64,
        absorption_id: &str,
        attestor: &str,
        ts: DateTime<Utc>,
        witnesses: &[&str],
    ) -> AttestorEnvelope {
        env(
            seq,
            ts,
            AttestorEventV1::Absorption {
                absorption_id: absid(absorption_id),
                attestor: aid(attestor),
                vertical: "physics".into(),
                t: ts,
                witnesses: witnesses.iter().copied().map(aid).collect(),
                public_artefact_uri: "https://example.org/x".into(),
            },
        )
    }

    // -- MycelialGate ----------------------------------------------------

    #[test]
    fn mycelial_gate_clean_two_witnesses() {
        let log = vec![absorb(0, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"])];
        let r = audit_mycelial_gate(&log);
        assert!(r.is_clean(), "{:?}", r.drifts);
    }

    #[test]
    fn mycelial_gate_flags_insufficient_witnesses() {
        let log = vec![absorb(0, "abs-1", "a0", t(2026, 5, 16), &["a1"])];
        let r = audit_mycelial_gate(&log);
        assert_eq!(r.drifts.len(), 1);
        match &r.drifts[0] {
            Drift::SpecInvariantViolation {
                invariant, spec, ..
            } => {
                assert_eq!(invariant, "insufficient_witnesses");
                assert_eq!(spec, "mycelial-gate");
            }
            d => panic!("unexpected drift {d:?}"),
        }
    }

    #[test]
    fn mycelial_gate_flags_self_witness() {
        let log = vec![absorb(
            0,
            "abs-1",
            "a0",
            t(2026, 5, 16),
            &["a0", "a1", "a2"],
        )];
        let r = audit_mycelial_gate(&log);
        // Self-witness drift is one finding; the threshold check uses
        // distinct-non-self count which is 2 here → no insufficient drift.
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, .. } if invariant == "self_witness")));
    }

    #[test]
    fn mycelial_gate_flags_duplicate_witnesses() {
        let log = vec![absorb(
            0,
            "abs-1",
            "a0",
            t(2026, 5, 16),
            &["a1", "a1", "a2"],
        )];
        let r = audit_mycelial_gate(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, .. } if invariant == "duplicate_witness")));
    }

    // -- AttestorGraph ---------------------------------------------------

    #[test]
    fn attestor_graph_clean_when_enrolments_metadata_match() {
        let log = vec![
            enrol(0, "a0", t(2026, 1, 1)),
            enrol(1, "a1", t(2026, 1, 1)),
            enrol(2, "a2", t(2026, 1, 1)),
            cluster(3, "a1", t(2026, 1, 2)),
            cluster(4, "a2", t(2026, 1, 2)),
            absorb(5, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_attestor_graph(&log);
        assert!(r.is_clean(), "{:?}", r.drifts);
    }

    #[test]
    fn attestor_graph_flags_witness_not_enrolled() {
        let log = vec![
            enrol(0, "a0", t(2026, 1, 1)),
            enrol(1, "a1", t(2026, 1, 1)),
            cluster(2, "a1", t(2026, 1, 2)),
            // a2 never enrolled
            absorb(3, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_attestor_graph(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, .. } if invariant == "witness_not_enrolled")));
    }

    #[test]
    fn attestor_graph_flags_witness_expired() {
        let log = vec![
            enrol(0, "a0", t(2026, 1, 1)),
            enrol(1, "a1", t(2026, 1, 1)),
            enrol(2, "a2", t(2026, 1, 1)),
            cluster(3, "a1", t(2026, 1, 2)),
            cluster(4, "a2", t(2026, 1, 2)),
            env(
                5,
                t(2026, 3, 1),
                AttestorEventV1::AttestorExpired {
                    attestor: aid("a1"),
                    t: t(2026, 3, 1),
                },
            ),
            absorb(6, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_attestor_graph(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, .. } if invariant == "witness_expired")));
    }

    #[test]
    fn attestor_graph_flags_audit_outcome_orphan() {
        let log = vec![env(
            0,
            t(2026, 5, 16),
            AttestorEventV1::AuditOutcome {
                absorption_id: absid("abs-unknown"),
                outcome: crate::attestor_event_v1::AuditOutcome::Ok,
                t: t(2026, 5, 16),
            },
        )];
        let r = audit_attestor_graph(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, .. } if invariant == "audit_outcome_orphan")));
    }

    // -- WitnessFreshness ------------------------------------------------

    #[test]
    fn witness_freshness_clean_fresh_metadata() {
        let log = vec![
            cluster(0, "a1", t(2026, 1, 1)),
            cluster(1, "a2", t(2026, 1, 1)),
            absorb(2, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_witness_freshness(&log);
        assert!(r.is_clean(), "{:?}", r.drifts);
    }

    #[test]
    fn witness_freshness_flags_stale_metadata() {
        let log = vec![
            cluster(0, "a1", t(2024, 1, 1)), // 2+ years old
            cluster(1, "a2", t(2026, 1, 1)),
            absorb(2, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_witness_freshness(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, subject, .. } if invariant == "stale_metadata" && subject.as_deref() == Some("a1"))));
    }

    #[test]
    fn witness_freshness_flags_no_metadata_in_window() {
        let log = vec![
            cluster(0, "a1", t(2026, 1, 1)),
            // a2 has no metadata at all
            absorb(1, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ];
        let r = audit_witness_freshness(&log);
        assert!(r
            .drifts
            .iter()
            .any(|d| matches!(d, Drift::SpecInvariantViolation { invariant, subject, .. } if invariant == "no_metadata_in_window" && subject.as_deref() == Some("a2"))));
    }
}
