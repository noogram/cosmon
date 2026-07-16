// SPDX-License-Identifier: AGPL-3.0-only

//! Shared finding types for `cs doctor` security probes.
//!
//! Each probe returns a `Vec<Finding>`. The dispatcher prints them
//! (text or JSON) and exits non-zero when any `Severity::Error` is present.
//!
//! Severity semantics are load-bearing — `Error` is the contract with CI:
//! a failing probe must break the build so that accidentally committed
//! secrets or world-writable worktrees never reach `main`.

use std::path::PathBuf;

use serde::Serialize;

/// Severity of a finding.
///
/// - `Error` — blocking; causes `cs doctor` to exit non-zero.
/// - `Warning` — flag for operator attention; exit zero.
/// - `Info` — informational only (e.g., how many files scanned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Blocking finding — probe refuses to pass.
    Error,
    /// Non-blocking but operator-visible.
    Warning,
    /// Informational status line.
    Info,
}

impl Severity {
    /// Upper-case short label used in text output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARN",
            Self::Info => "INFO",
        }
    }
}

/// Single structured finding emitted by a probe.
///
/// The text output uses `probe`, `severity`, `title`, and optionally
/// `path`/`remediation`/`detail`. JSON output serializes all fields.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Short probe name (`leaks`, `worktrees`, `mcp`, `deps`, …).
    pub probe: &'static str,
    /// Severity bucket.
    pub severity: Severity,
    /// One-line human title (what was found).
    pub title: String,
    /// Optional longer detail (line numbers, match snippets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional offending path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Optional remediation hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Finding {
    /// Construct a new finding with just probe/severity/title.
    #[must_use]
    pub fn new(probe: &'static str, severity: Severity, title: impl Into<String>) -> Self {
        Self {
            probe,
            severity,
            title: title.into(),
            detail: None,
            path: None,
            remediation: None,
        }
    }

    /// Attach an offending path.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Attach a remediation hint.
    #[must_use]
    pub fn with_remediation(mut self, hint: impl Into<String>) -> Self {
        self.remediation = Some(hint.into());
        self
    }

    /// Attach a longer detail string.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Report produced by a single probe run.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    /// Probe name.
    pub probe: &'static str,
    /// Total number of items scanned (informational).
    pub scanned: usize,
    /// Findings, ordered by severity (errors first).
    pub findings: Vec<Finding>,
}

impl ProbeReport {
    /// New empty report for a probe.
    #[must_use]
    pub fn new(probe: &'static str) -> Self {
        Self {
            probe,
            scanned: 0,
            findings: Vec::new(),
        }
    }

    /// `true` if any finding is an `Error`.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Count of findings at the given severity.
    #[must_use]
    pub fn count(&self, sev: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == sev).count()
    }
}

/// Print a human-readable summary of one or more probe reports.
///
/// Returns `true` if any report has errors (caller should exit non-zero).
pub fn render_text(reports: &[ProbeReport]) -> bool {
    let mut any_errors = false;
    for r in reports {
        let errors = r.count(Severity::Error);
        let warnings = r.count(Severity::Warning);
        println!(
            "probe {}: scanned={} errors={} warnings={}",
            r.probe, r.scanned, errors, warnings
        );
        for f in &r.findings {
            print_finding(f);
        }
        if r.has_errors() {
            any_errors = true;
        }
    }
    any_errors
}

fn print_finding(f: &Finding) {
    println!("  [{}] {}: {}", f.severity.label(), f.probe, f.title);
    if let Some(p) = &f.path {
        println!("       path: {}", p.display());
    }
    if let Some(d) = &f.detail {
        for line in d.lines() {
            println!("       {line}");
        }
    }
    if let Some(r) = &f.remediation {
        println!("       hint: {r}");
    }
}
