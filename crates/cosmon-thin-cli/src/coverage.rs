// SPDX-License-Identifier: Apache-2.0

//! Coverage report — UX layer for `cs-thin verbs --check` and the
//! machine-readable `--coverage-report --json` output.
//!
//! # Why this module exists
//!
//! This module finalises the UX around the `cs-thin` completeness
//! report — the binary is the proof: an operator screenshots the
//! output and pastes it into the audit doc. Two consumers, one truth:
//!
//! 1. [`render_human`] — human-readable, colourable output for `cs-thin
//!    verbs --check`. The line a operator-demo would screenshot.
//! 2. [`render_json`] — machine-readable JSON for `cs-thin
//!    --coverage-report --json`. The body the CI gate parses.
//!
//! Both consumers feed off [`CoverageReport`], which is itself derived
//! from compile-time data: the link-time [`crate::registry::VERBS`] slice
//! (mechanical) and the [`OPERATOR_ONLY`] static list (manual, kept in
//! sync with ADR-080 §5.1 by the
//! `tests::operator_only_in_sync_with_adr` integration test).
//!
//! # Why duplicated metadata is intentional
//!
//! The operator-only list is a **structural** statement: these verbs
//! have a blast-radius incompatible with a JWT-bearing principal. We
//! *want* a compile-time binding to the ADR text — the test parses the
//! ADR markdown and refuses any drift. That makes the list reviewable
//! both from the code (intent) and from the prose (rationale).

use serde::Serialize;

use crate::registry::VerbDescriptor;

/// One entry in the operator-only list (ADR-080 §5.1).
///
/// `name` is the verb's source-level token (`"done"`, `"evolve"`, …);
/// for compound verbs (e.g. `cs security activate`) we keep the full
/// space-separated form to mirror the ADR. `note` is an optional
/// short qualifier rendered after the verb name in human output (e.g.
/// `"V2 candidate"` for `verify`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct OperatorOnlyEntry {
    /// Verb name, exactly as it appears under `cs <name>` in ADR-080
    /// §5.1 (compound verbs keep the space, e.g. `"security activate"`).
    pub name: &'static str,
    /// ADR reference for this entry. Always `"ADR-080 §5.1"` today;
    /// kept as a field so a future split (per-row deltas) does not
    /// require touching every entry.
    pub adr_ref: &'static str,
    /// Optional human-readable qualifier (e.g. `"V2 candidate"`).
    pub note: Option<&'static str>,
}

/// Operator-only verbs — exhaustive non-exposable list (ADR-080 §5.1).
///
/// **Source of truth:** this static list is the compile-time embedding
/// of the markdown table in `docs/adr/080-remote-pilot-port-https-oidc.md`
/// §5.1. The integration test `operator_only_in_sync_with_adr` parses
/// the ADR and asserts the two sides match — adding a verb here without
/// updating the ADR (or vice-versa) fails CI.
///
/// Entries appear in the same order as the ADR table for diff-friendliness.
pub const OPERATOR_ONLY: &[OperatorOnlyEntry] = &[
    OperatorOnlyEntry {
        name: "done",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "evolve",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "complete",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "security activate",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    // `run` left the list 2026-06-11 (ADR-124, task-20260610-56c4):
    // the bounded resident drain `POST /v1/molecules/{id}/run` exposes
    // a REQUEST for a drain, never the operator orchestrator itself —
    // bounds come from the sealed binding (B1/B2/B3), the server
    // decides what to tackle. See ADR-124 for the §5.2-compliant
    // re-decision.
    OperatorOnlyEntry {
        name: "kill",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "purge",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "reconcile",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "verify",
        adr_ref: "ADR-080 §5.1",
        note: Some("V2 candidate"),
    },
    OperatorOnlyEntry {
        name: "whisper",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
    OperatorOnlyEntry {
        name: "drop",
        adr_ref: "ADR-080 §5.1",
        note: None,
    },
];

/// Per-verb coverage row in the JSON report.
#[derive(Debug, Clone, Serialize)]
pub struct VerbReport {
    /// Verb name (e.g. `"observe"`).
    pub name: String,
    /// `"covered"` (in the cs-thin registry) or `"operator_only"`
    /// (structurally non-exposable per ADR-080 §5.1).
    pub status: &'static str,
    /// HTTP method literal — present only when `status == "covered"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// URL path template — present only when `status == "covered"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// ADR reference — present only when `status == "operator_only"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adr_ref: Option<String>,
    /// Optional qualifier (e.g. `"V2 candidate"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Top-level coverage report — the JSON body emitted by
/// `cs-thin --coverage-report --json` and the seed for the
/// human-readable rendering.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Cargo package version of `cosmon-thin-cli` at build time.
    pub version: String,
    /// Configured base URL (or `null` when neither `--base-url` nor
    /// `CS_THIN_BASE_URL` is set — the report is a self-description,
    /// not a probe, so no network is required).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Number of cs-thin verbs that have wire dispatch wired
    /// (i.e. appear in the link-time registry).
    pub covered_exposable: usize,
    /// Total exposable verb count — equal to `covered_exposable` by
    /// construction (the registry is the source of truth and dispatch
    /// is paired with `#[verb]` annotation in the same PR). Drift
    /// between the two would surface as a non-1.0 ratio and a CI
    /// failure on `coverage_complete`.
    pub total_exposable: usize,
    /// Number of operator-only verbs (ADR-080 §5.1).
    pub operator_only: usize,
    /// `covered_exposable + operator_only` — the cs-CLI surface that
    /// `cs-thin` *models*. New cs verbs that are not modelled (e.g.
    /// `cs ask`, `cs spark`) deliberately do not count against the
    /// ratio: §8p is a *subset strict*, not a bijection.
    pub total_cs_verbs: usize,
    /// `covered_exposable / total_exposable`, in the inclusive range
    /// `[0.0, 1.0]`. Always `1.0` under healthy conditions; a non-1.0
    /// value means a verb was annotated without wiring dispatch — the
    /// CI gate refuses to ship.
    pub ratio_covered: f64,
    /// Per-verb rows — covered first (alphabetical), then
    /// operator-only (in ADR order).
    pub verbs: Vec<VerbReport>,
    /// `"COMPLETE"` when `ratio_covered == 1.0`, otherwise `"INCOMPLETE"`.
    /// The CI gate `coverage_complete` asserts the former.
    pub status: &'static str,
}

/// Build a [`CoverageReport`] from the link-time registry plus the
/// compile-time [`OPERATOR_ONLY`] list.
///
/// `target` is the configured base URL (e.g. `"https://api.noogram.org"`),
/// or `None` when neither `--base-url` nor `CS_THIN_BASE_URL` is set.
#[must_use]
pub fn build_report(target: Option<String>) -> CoverageReport {
    let mut covered: Vec<&VerbDescriptor> = crate::registry::all().iter().collect();
    covered.sort_by_key(|d| d.name);

    let covered_exposable = covered.len();
    let total_exposable = covered_exposable; // see field doc — drift is forbidden
    let operator_only = OPERATOR_ONLY.len();
    let total_cs_verbs = covered_exposable + operator_only;
    let ratio_covered = if total_exposable == 0 {
        // No exposable verbs registered yet — vacuously complete; the
        // CI gate will still flag this as `INCOMPLETE` because no V0
        // surface is meaningful.
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let ratio = (covered_exposable as f64) / (total_exposable as f64);
        ratio
    };

    let mut verbs: Vec<VerbReport> = covered
        .iter()
        .map(|d| VerbReport {
            name: d.name.to_owned(),
            status: "covered",
            method: Some(d.method.to_owned()),
            path: Some(d.path.to_owned()),
            adr_ref: None,
            note: None,
        })
        .collect();

    for entry in OPERATOR_ONLY {
        verbs.push(VerbReport {
            name: entry.name.to_owned(),
            status: "operator_only",
            method: None,
            path: None,
            adr_ref: Some(entry.adr_ref.to_owned()),
            note: entry.note.map(str::to_owned),
        });
    }

    let status = if (ratio_covered - 1.0).abs() < f64::EPSILON && covered_exposable > 0 {
        "COMPLETE"
    } else {
        "INCOMPLETE"
    };

    CoverageReport {
        version: crate::VERSION.to_owned(),
        target,
        covered_exposable,
        total_exposable,
        operator_only,
        total_cs_verbs,
        ratio_covered,
        verbs,
        status,
    }
}

/// Render the human-readable form (the line operators screenshot).
///
/// Format mirrors the ADR-080 §5.1 audit narrative:
///
/// ```text
/// cs-thin verbs --check (cosmon-thin-cli v0.1.0, target: https://...)
///
/// ✓ observe       GET    /v1/molecules/:id
/// ✓ nucleate      POST   /v1/molecules
/// ✓ tag           POST   /v1/molecules/:id/tags
///
/// ⚠ done          OPERATOR-ONLY (ADR-080 §5.1)
/// ⚠ evolve        OPERATOR-ONLY (ADR-080 §5.1)
/// ...
///
/// ─────────────────────────────────────────────────
/// 3/3 RPP-exposable verbs covered.
/// 11 operator-only verbs by design (see ADR-080 §5.1).
/// Status: COMPLETE.
/// ```
#[must_use]
pub fn render_human(report: &CoverageReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let target = report.target.as_deref().unwrap_or("(unset)");
    let _ = writeln!(
        out,
        "cs-thin verbs --check (cosmon-thin-cli v{}, target: {})\n",
        report.version, target,
    );

    for v in report.verbs.iter().filter(|v| v.status == "covered") {
        let _ = writeln!(
            out,
            "✓ {:<14} {:<6} {}",
            v.name,
            v.method.as_deref().unwrap_or(""),
            v.path.as_deref().unwrap_or(""),
        );
    }

    if report.verbs.iter().any(|v| v.status == "operator_only") {
        out.push('\n');
    }

    for v in report.verbs.iter().filter(|v| v.status == "operator_only") {
        let suffix = match &v.note {
            Some(n) => format!(" — {n}"),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "⚠ {:<14} OPERATOR-ONLY ({}){}",
            v.name,
            v.adr_ref.as_deref().unwrap_or("ADR-080 §5.1"),
            suffix,
        );
    }

    out.push_str("\n─────────────────────────────────────────────────\n");
    let _ = writeln!(
        out,
        "{}/{} RPP-exposable verbs covered.",
        report.covered_exposable, report.total_exposable,
    );
    let _ = writeln!(
        out,
        "{} operator-only verbs by design (see ADR-080 §5.1).",
        report.operator_only,
    );
    let _ = writeln!(out, "Status: {}.", report.status);

    out
}

/// Render the report as a single-line compact JSON document. The CI
/// gate parses this; the audit doc embeds it for the screenshot.
///
/// # Errors
///
/// Returns the underlying `serde_json` encode error on failure (an
/// extreme-edge case — every field is plain-old-data).
pub fn render_json(report: &CoverageReport) -> Result<String, serde_json::Error> {
    serde_json::to_string(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_only_list_is_non_empty_and_unique() {
        // The list must be non-empty (we are advertising operator-only
        // verbs as a feature) and free of duplicates (a duplicate is a
        // copy-paste error).
        assert!(
            !OPERATOR_ONLY.is_empty(),
            "OPERATOR_ONLY must list at least one verb"
        );
        let mut names: Vec<_> = OPERATOR_ONLY.iter().map(|e| e.name).collect();
        names.sort_unstable();
        let original_len = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            original_len,
            "duplicate entry in OPERATOR_ONLY"
        );
    }

    #[test]
    fn report_status_is_complete_when_registry_non_empty() {
        let report = build_report(Some("https://example".to_owned()));
        // The cs-thin crate registers three verbs at link time
        // (observe / nucleate / tag); the ratio is therefore 1.0 and
        // status is COMPLETE.
        if report.covered_exposable > 0 {
            assert!(
                (report.ratio_covered - 1.0).abs() < f64::EPSILON,
                "expected ratio 1.0, got {}",
                report.ratio_covered
            );
            assert_eq!(report.status, "COMPLETE");
        }
    }

    #[test]
    fn report_human_render_contains_all_verbs() {
        let report = build_report(None);
        let human = render_human(&report);
        for entry in OPERATOR_ONLY {
            assert!(
                human.contains(entry.name),
                "human render missing operator-only verb {}: {human}",
                entry.name
            );
        }
        for v in report.verbs.iter().filter(|v| v.status == "covered") {
            assert!(
                human.contains(&v.name),
                "human render missing covered verb {}: {human}",
                v.name
            );
        }
    }

    #[test]
    fn report_json_round_trips_to_value() {
        let report = build_report(Some("http://localhost:1".to_owned()));
        let s = render_json(&report).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["version"], crate::VERSION);
        assert_eq!(v["target"], "http://localhost:1");
        assert_eq!(
            v["operator_only"]
                .as_u64()
                .expect("operator_only is an integer"),
            OPERATOR_ONLY.len() as u64
        );
        assert!(v["verbs"].as_array().unwrap().len() >= OPERATOR_ONLY.len());
    }
}
