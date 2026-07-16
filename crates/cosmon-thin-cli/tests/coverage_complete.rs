// SPDX-License-Identifier: Apache-2.0

//! CI gate — `cs-thin --coverage-report --json` must report
//! `ratio_covered == 1.0` and `status == "COMPLETE"`.
//!
//! The structural intent: every verb annotated with `#[verb]` in
//! `cosmon-thin-cli::verbs::*` must have a working dispatch arm in
//! [`cosmon_thin_cli::cli::Command`]. Drift in either direction (a
//! verb annotated without dispatch, dispatch without annotation)
//! surfaces as a non-1.0 ratio and fails this gate.
//!
//! We drive the binary in-process via [`cosmon_thin_cli::cli::run_with`]
//! rather than spawning the compiled `cs-thin` binary — same byte
//! output, no `cargo run` round trip, faster CI.

use cosmon_thin_cli::cli::{run_with, Cli};
use serde_json::Value;

/// Build a `Cli` with `--coverage-report --json` set and no
/// subcommand. Mirrors the operator command-line invocation
/// `cs-thin --coverage-report --json`.
fn coverage_cli() -> Cli {
    Cli {
        base_url: None,
        jwt_from_env: None,
        jwt_file: None,
        coverage_report: true,
        json: true,
        command: None,
    }
}

#[tokio::test]
async fn coverage_report_is_valid_json() {
    let cli = coverage_cli();
    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("--coverage-report should succeed offline");
    let s = std::str::from_utf8(&out).unwrap().trim();
    let _: Value = serde_json::from_str(s).expect("output must be parseable JSON");
}

#[tokio::test]
async fn coverage_report_is_complete() {
    let cli = coverage_cli();
    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("--coverage-report should succeed offline");
    let s = std::str::from_utf8(&out).unwrap().trim();
    let v: Value = serde_json::from_str(s).expect("valid JSON");

    // Headline assertions — these are what the CI gate actually
    // protects.
    let ratio = v["ratio_covered"]
        .as_f64()
        .expect("ratio_covered is a float");
    assert!(
        (ratio - 1.0).abs() < f64::EPSILON,
        "ratio_covered must be 1.0, got {ratio} — \
         a verb is annotated but missing dispatch (or vice-versa). \
         Inspect `cs-thin verbs --check` for the offending row."
    );
    assert_eq!(
        v["status"], "COMPLETE",
        "coverage status must be COMPLETE — the rapport is the proof"
    );

    // Defence-in-depth: cross-check the structural counts so a future
    // refactor that accidentally zeroes one side without zeroing the
    // other is caught here too.
    let covered = v["covered_exposable"].as_u64().expect("u64");
    let total = v["total_exposable"].as_u64().expect("u64");
    let operator_only = v["operator_only"].as_u64().expect("u64");
    let total_cs = v["total_cs_verbs"].as_u64().expect("u64");
    assert_eq!(covered, total, "covered_exposable == total_exposable");
    assert_eq!(
        total_cs,
        covered + operator_only,
        "total_cs_verbs == covered_exposable + operator_only"
    );
    assert!(
        covered > 0,
        "no exposable verbs registered — V0 must wire at least one"
    );
}

#[tokio::test]
async fn coverage_report_lists_every_modelled_verb() {
    let cli = coverage_cli();
    let mut out = Vec::new();
    run_with(cli, &mut out).await.expect("ok");
    let s = std::str::from_utf8(&out).unwrap().trim();
    let v: Value = serde_json::from_str(s).expect("valid JSON");

    let verbs = v["verbs"].as_array().expect("verbs is an array");
    let total_cs = v["total_cs_verbs"].as_u64().unwrap();
    assert_eq!(
        verbs.len() as u64,
        total_cs,
        "verbs[] length must equal total_cs_verbs"
    );

    // Every row carries either (method,path) for covered or adr_ref
    // for operator_only — never both, never neither.
    for row in verbs {
        let status = row["status"].as_str().expect("status is a string");
        match status {
            "covered" => {
                assert!(row.get("method").is_some(), "covered row missing method");
                assert!(row.get("path").is_some(), "covered row missing path");
                assert!(
                    row.get("adr_ref").is_none(),
                    "covered row must not carry adr_ref"
                );
            }
            "operator_only" => {
                assert!(
                    row.get("adr_ref").is_some(),
                    "operator_only row missing adr_ref"
                );
                assert!(
                    row.get("method").is_none(),
                    "operator_only row must not carry method"
                );
            }
            other => panic!("unknown status `{other}` in row {row}"),
        }
    }
}
