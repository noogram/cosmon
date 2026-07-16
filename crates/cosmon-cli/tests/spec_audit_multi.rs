// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end regression — `cs spec-audit --spec <noogram-spec>`.
//!
//! Exercises the multi-spec path.
//! Each test writes a minimal `attestor-events.jsonl` ledger and runs
//! `cs spec-audit --spec <name> --events <path>`, then asserts on the
//! shape of the report.
//!
//! The c1cb behaviour (default mode, `EventV2` ledger) is locked in by
//! `spec_audit_c1cb.rs`; this test file covers the new auditors.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, TimeZone, Utc};
use cosmon_core::attestor_event_v1::{AbsorptionId, AttestorEnvelope, AttestorEventV1, AttestorId};

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn t(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, 12, 0, 0).unwrap()
}

fn aid(s: &str) -> AttestorId {
    AttestorId::new(s)
}
fn absid(s: &str) -> AbsorptionId {
    AbsorptionId::new(s)
}

fn write_attestor_events(path: &Path, envelopes: &[AttestorEnvelope]) {
    let mut f = fs::File::create(path).unwrap();
    for env in envelopes {
        writeln!(f, "{}", serde_json::to_string(env).unwrap()).unwrap();
    }
}

fn run_audit(spec: &str, events: &Path) -> (bool, serde_json::Value, String) {
    let output = cosmon_bin()
        .arg("--json")
        .arg("spec-audit")
        .arg("--spec")
        .arg(spec)
        .arg("--events")
        .arg(events)
        .output()
        .expect("failed to invoke cs spec-audit");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout is not JSON: error={e}; stdout={stdout}; stderr={stderr}");
    });
    (output.status.success(), report, stderr)
}

// --------------------------------------------------------------------------
// MycelialGate
// --------------------------------------------------------------------------

#[test]
fn spec_audit_mycelial_gate_clean_two_witnesses() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[AttestorEnvelope::new(
            0,
            t(2026, 5, 16),
            AttestorEventV1::Absorption {
                absorption_id: absid("abs-1"),
                attestor: aid("a0"),
                vertical: "physics".into(),
                t: t(2026, 5, 16),
                witnesses: vec![aid("a1"), aid("a2")],
                public_artefact_uri: "https://example.org/p".into(),
            },
        )],
    );
    let (ok, report, _stderr) = run_audit("mycelial-gate", &p);
    assert!(ok, "expected clean; report={report}");
    assert_eq!(report["drifts"].as_array().unwrap().len(), 0);
}

#[test]
fn spec_audit_mycelial_gate_flags_insufficient_witnesses() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[AttestorEnvelope::new(
            0,
            t(2026, 5, 16),
            AttestorEventV1::Absorption {
                absorption_id: absid("abs-1"),
                attestor: aid("a0"),
                vertical: "physics".into(),
                t: t(2026, 5, 16),
                witnesses: vec![aid("a1")], // only one witness
                public_artefact_uri: "https://example.org/p".into(),
            },
        )],
    );
    let (ok, report, _stderr) = run_audit("mycelial-gate", &p);
    assert!(!ok, "expected drift; report={report}");
    let drifts = report["drifts"].as_array().unwrap();
    let drift = drifts
        .iter()
        .find(|d| d["kind"] == "spec_invariant_violation")
        .expect("expected spec_invariant_violation drift");
    assert_eq!(drift["spec"], "mycelial-gate");
    assert_eq!(drift["invariant"], "insufficient_witnesses");
}

// --------------------------------------------------------------------------
// AttestorGraph
// --------------------------------------------------------------------------

fn enrol(seq: u64, a: &str, ts: DateTime<Utc>) -> AttestorEnvelope {
    AttestorEnvelope::new(
        seq,
        ts,
        AttestorEventV1::AttestorEnrol {
            attestor: aid(a),
            t: ts,
        },
    )
}

fn cluster(seq: u64, a: &str, ts: DateTime<Utc>) -> AttestorEnvelope {
    AttestorEnvelope::new(
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
    AttestorEnvelope::new(
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

#[test]
fn spec_audit_attestor_graph_clean_full_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[
            enrol(0, "a0", t(2026, 1, 1)),
            enrol(1, "a1", t(2026, 1, 1)),
            enrol(2, "a2", t(2026, 1, 1)),
            cluster(3, "a1", t(2026, 1, 2)),
            cluster(4, "a2", t(2026, 1, 2)),
            absorb(5, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ],
    );
    let (ok, report, _stderr) = run_audit("attestor-graph", &p);
    assert!(ok, "expected clean; report={report}");
}

#[test]
fn spec_audit_attestor_graph_flags_witness_not_enrolled() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[
            enrol(0, "a0", t(2026, 1, 1)),
            enrol(1, "a1", t(2026, 1, 1)),
            cluster(2, "a1", t(2026, 1, 2)),
            // a2 never enrolled
            absorb(3, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ],
    );
    let (ok, report, _stderr) = run_audit("attestor-graph", &p);
    assert!(!ok, "expected drift");
    let drifts = report["drifts"].as_array().unwrap();
    assert!(drifts
        .iter()
        .any(|d| d["spec"] == "attestor-graph" && d["invariant"] == "witness_not_enrolled"));
}

// --------------------------------------------------------------------------
// WitnessFreshness
// --------------------------------------------------------------------------

#[test]
fn spec_audit_witness_freshness_clean_fresh_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[
            cluster(0, "a1", t(2026, 1, 1)),
            cluster(1, "a2", t(2026, 1, 1)),
            absorb(2, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ],
    );
    let (ok, report, _stderr) = run_audit("witness-freshness", &p);
    assert!(ok, "expected clean; report={report}");
}

#[test]
fn spec_audit_witness_freshness_flags_stale_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(
        &p,
        &[
            cluster(0, "a1", t(2024, 1, 1)), // 2+ years old
            cluster(1, "a2", t(2026, 1, 1)),
            absorb(2, "abs-1", "a0", t(2026, 5, 16), &["a1", "a2"]),
        ],
    );
    let (ok, report, _stderr) = run_audit("witness-freshness", &p);
    assert!(!ok, "expected drift");
    let drifts = report["drifts"].as_array().unwrap();
    assert!(drifts
        .iter()
        .any(|d| d["spec"] == "witness-freshness" && d["invariant"] == "stale_metadata"));
}

// --------------------------------------------------------------------------
// Spec selection: .tla path acceptance, default fall-through, unknown reject
// --------------------------------------------------------------------------

#[test]
fn spec_audit_accepts_tla_path_for_spec_arg() {
    // `--spec noogram/specs/MycelialGate.tla` must resolve to the
    // mycelial-gate auditor (basename → registry key).
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    write_attestor_events(&p, &[absorb(0, "abs-1", "a0", t(2026, 5, 16), &["a1"])]);

    let output = cosmon_bin()
        .arg("--json")
        .arg("spec-audit")
        .arg("--spec")
        .arg("noogram/specs/MycelialGate.tla")
        .arg("--events")
        .arg(&p)
        .output()
        .expect("invocation failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "expected drift for single-witness absorption"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let drifts = report["drifts"].as_array().unwrap();
    assert!(drifts
        .iter()
        .any(|d| d["spec"] == "mycelial-gate" && d["invariant"] == "insufficient_witnesses"));
}

#[test]
fn spec_audit_rejects_unknown_spec_name() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("attestor.jsonl");
    fs::write(&p, "").unwrap();
    let output = cosmon_bin()
        .arg("spec-audit")
        .arg("--spec")
        .arg("NotARealSpec")
        .arg("--events")
        .arg(&p)
        .output()
        .expect("invocation failed");
    assert!(
        !output.status.success(),
        "expected non-zero exit for unknown spec"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown --spec"),
        "stderr should mention unknown spec; got: {stderr}"
    );
}

#[test]
fn spec_audit_default_spec_is_cosmon_run() {
    // No --spec → must read events.jsonl (EventV2 format) and act as
    // before. We supply an empty events file: that is a clean audit.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("events.jsonl");
    fs::write(&p, "").unwrap();

    let output = cosmon_bin()
        .arg("--json")
        .arg("spec-audit")
        .arg("--events")
        .arg(&p)
        .arg("--no-git-probe")
        .output()
        .expect("invocation failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "empty ledger should be clean; stdout={stdout}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["drifts"].as_array().unwrap().len(), 0);
    assert_eq!(report["events_replayed"].as_u64(), Some(0));
}
