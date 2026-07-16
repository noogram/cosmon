// SPDX-License-Identifier: AGPL-3.0-only

//! Types and pure helpers for the `nucleon-test` formula.
//!
//! The formula has three steps — `scan`, `probe`, `report` — and this
//! module gives each step the types it needs. All helpers are I/O-free;
//! the worker path supplies a pre-collected [`NucleonScan`] and the
//! helpers produce a [`ProbeOutput`] and finally a [`NucleonReport`]
//! that can be rendered to markdown.
//!
//! # Discipline
//!
//! - Every helper is a pure function of the scan inventory. No
//!   filesystem reads, no clock access, no network.
//! - A test that cannot be evaluated returns [`Verdict::Inconclusive`]
//!   with a reason, never [`Verdict::Fail`]. The formula is a telescope,
//!   not a gate.
//! - `ADMITTED` requires **all** of the load-bearing subset
//!   {T1, T2, T4, T5, T6} to pass AND no violated guarantees (see
//!   ADR-066 §(4)). Any `Fail` in that subset produces `DEFERRED`;
//!   `Inconclusive` is not admissible either.
//! - The report never mutates another molecule, never blocks, and
//!   never collapses. It is one markdown artifact in the formula's
//!   own molecule directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test and guarantee identifiers
// ---------------------------------------------------------------------------

/// The seven behavior tests (T1..T7) ratified in ADR-066 §(4).
///
/// The variants match the briefing order — [`TestId::T1`] through
/// [`TestId::T7`] — so the enum discriminant equals the test number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TestId {
    /// T1 — causal trace on disk (materialize-before-write).
    T1,
    /// T2 — append-only + stable id + sealed.
    T2,
    /// T3 — observability from `.cosmon/` (sparks are traceable).
    T3,
    /// T4 — stable continuity id + human-readable prose.
    T4,
    /// T5 — decidable authorship at every spark.
    T5,
    /// T6 — bounded ingress contract + non-corruption of peers.
    T6,
    /// T7 — adversarial-distinguishability + graceful silence.
    T7,
}

impl TestId {
    /// Ordered list of every test, T1..T7. Useful for iteration.
    #[must_use]
    pub const fn all() -> [TestId; 7] {
        [
            TestId::T1,
            TestId::T2,
            TestId::T3,
            TestId::T4,
            TestId::T5,
            TestId::T6,
            TestId::T7,
        ]
    }

    /// Short label, e.g. `"T1"`. Matches the report rendering.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TestId::T1 => "T1",
            TestId::T2 => "T2",
            TestId::T3 => "T3",
            TestId::T4 => "T4",
            TestId::T5 => "T5",
            TestId::T6 => "T6",
            TestId::T7 => "T7",
        }
    }

    /// Load-bearing subset per ADR-066 §(4): {T1, T2, T4, T5, T6}.
    /// Every test in this subset must `Pass` for `ADMITTED`.
    #[must_use]
    pub fn load_bearing() -> [TestId; 5] {
        [TestId::T1, TestId::T2, TestId::T4, TestId::T5, TestId::T6]
    }

    /// Whether this test is in the load-bearing subset.
    #[must_use]
    pub fn is_load_bearing(self) -> bool {
        matches!(
            self,
            TestId::T1 | TestId::T2 | TestId::T4 | TestId::T5 | TestId::T6
        )
    }
}

impl fmt::Display for TestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The three observable guarantees (G1..G3): continuity,
/// non-retroactivity, and legibility of every nucleated spark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GuaranteeId {
    /// G1 — continuity (no overlapping sessions for one `nucleon_id`).
    G1,
    /// G2 — non-retroactivity (no cross-nucleon writes).
    G2,
    /// G3 — legibility (cognitive context reconstructable from disk).
    G3,
}

impl GuaranteeId {
    /// Ordered list of every guarantee, G1..G3.
    #[must_use]
    pub const fn all() -> [GuaranteeId; 3] {
        [GuaranteeId::G1, GuaranteeId::G2, GuaranteeId::G3]
    }

    /// Short label, e.g. `"G1"`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            GuaranteeId::G1 => "G1",
            GuaranteeId::G2 => "G2",
            GuaranteeId::G3 => "G3",
        }
    }
}

impl fmt::Display for GuaranteeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// Outcome of a single test or guarantee probe.
///
/// A test that cannot be evaluated returns [`Verdict::Inconclusive`],
/// never [`Verdict::Fail`] — the formula is a telescope, not a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The test's invariant holds on the cited evidence.
    Pass,
    /// The test's invariant is violated on the cited evidence.
    Fail,
    /// The test could not be evaluated (e.g. missing corpus). The
    /// reason must accompany the verdict in the result record.
    Inconclusive,
}

impl Verdict {
    /// Unicode glyph used in the rendered report: ✓ / ✗ / ⚠.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Verdict::Pass => "✓",
            Verdict::Fail => "✗",
            Verdict::Inconclusive => "⚠",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.glyph())
    }
}

// ---------------------------------------------------------------------------
// Scan output
// ---------------------------------------------------------------------------

/// Pre-collected footprint of a candidate Nucléon on disk.
///
/// The `scan` step walks `.cosmon/state/` and populates this structure;
/// `probe` and `report` consume it as a pure input. Keeping the scan
/// result as a data structure (not a filesystem handle) lets us
/// property-test the downstream helpers without touching disk.
///
/// The struct carries many booleans on purpose — each one is a distinct
/// invariant observable on disk (append-only, sealed, identity-present,
/// etc.) and collapsing them into a flag set would lose evidence-level
/// granularity the report needs to cite.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct NucleonScan {
    /// The `nucleon_id` of the candidate under audit.
    pub candidate: String,

    /// Optional free-form substrate name
    /// (e.g. `"apple-foundation-models-3b"`).
    #[serde(default)]
    pub substrate: Option<String>,

    /// Scan window in days. `0` means "all history".
    #[serde(default)]
    pub window_days: u32,

    /// Number of `pilot-session` molecules discovered whose
    /// `nucleon_id == candidate`.
    #[serde(default)]
    pub pilot_sessions: u32,

    /// Number of nucleated molecules whose `author_nucleon_id`
    /// resolves to `candidate`.
    #[serde(default)]
    pub authored_molecules: u32,

    /// Number of `SparkedBy` edges pointing into a session owned by
    /// `candidate`.
    #[serde(default)]
    pub sparked_edges: u32,

    /// Number of carnet entries under `.cosmon/state/sessions/` that
    /// carry a `cause:` subline (T5 evidence).
    #[serde(default)]
    pub carnet_entries_with_cause: u32,

    /// Total carnet entries under `.cosmon/state/sessions/` for this
    /// candidate. A gap between this and
    /// [`Self::carnet_entries_with_cause`] indicates legacy notes
    /// that are not admission-blocking but must be surfaced.
    #[serde(default)]
    pub carnet_entries_total: u32,

    /// Whether an `identity.toml` exists under
    /// `.cosmon/state/nucleons/<candidate>/`.
    #[serde(default)]
    pub identity_file_present: bool,

    /// Whether the identity file is sealed.
    #[serde(default)]
    pub identity_file_sealed: bool,

    /// Whether every discovered session carnet is append-only + sealed.
    #[serde(default)]
    pub carnets_append_only_sealed: bool,

    /// Distinct `nucleon_id` spellings observed across sessions. For
    /// T2 to pass, this set must contain exactly `{candidate}`. Any
    /// other element indicates silent rotation.
    #[serde(default)]
    pub observed_ids: BTreeSet<String>,

    /// `SparkedBy` integrity — `true` means every nucleated molecule
    /// has a `SparkedBy` edge; `false` means at least one orphan was
    /// found. A fresh candidate with zero molecules leaves this
    /// `true` (no orphan is possible).
    #[serde(default = "default_true")]
    pub sparked_by_complete: bool,

    /// Whether any `author_nucleon_id` writes were observed into
    /// another Nucléon's state tree (T6 peer-corruption).
    #[serde(default)]
    pub peer_corruption_detected: bool,

    /// Whether session carnets are in human-readable prose (T4).
    #[serde(default = "default_true")]
    pub prose_readable: bool,

    /// Whether the candidate is non-CLI and therefore requires a
    /// `<substrate>_event_to_spark` admission boundary. When `true`,
    /// [`Self::admission_boundary_present`] must also be `true` for
    /// T6 to pass.
    #[serde(default)]
    pub requires_admission_boundary: bool,

    /// Whether the admission boundary exists when required.
    #[serde(default)]
    pub admission_boundary_present: bool,

    /// Pairs of overlapping-in-wall-clock-time sessions for the same
    /// `nucleon_id`. Empty ⇒ G1 holds. Non-empty ⇒ G1 violated.
    #[serde(default)]
    pub session_overlaps: Vec<SessionOverlap>,

    /// Count of events attributed to the candidate that mutated a
    /// molecule outside its declared ancestry. `0` ⇒ G2 holds.
    #[serde(default)]
    pub cross_ancestry_writes: u32,

    /// Count of `SparkedBy` edges whose context cannot be
    /// reconstructed from `.cosmon/` alone. `0` ⇒ G3 holds.
    #[serde(default)]
    pub illegible_edges: u32,

    /// Duplicate nucleation count — re-issuing the same logical
    /// intent in one session must not double-nucleate. `0` ⇒ T5
    /// idempotency holds.
    #[serde(default)]
    pub duplicate_nucleations: u32,

    /// Whether the candidate nucleated a deliberation-of-deliberation
    /// without declaring a depth bound in `prompt.md`. Triggers the
    /// *unbounded-metacognition* excluded-substrate flag.
    #[serde(default)]
    pub unbounded_metacognition: bool,

    /// Whether the corpus contains a second carnet written by a
    /// competent human over the same window. Required for T7
    /// evaluation; absence ⇒ T7 inconclusive.
    #[serde(default)]
    pub reference_human_carnet_present: bool,

    /// Whether, given [`Self::reference_human_carnet_present`], the
    /// two carnets are distinguishable by a third-party verifier.
    /// Only inspected when the reference carnet exists.
    #[serde(default)]
    pub candidate_distinguishable_from_human: bool,
}

const fn default_true() -> bool {
    true
}

/// A wall-clock overlap between two pilot-sessions sharing a
/// `nucleon_id`. Reported verbatim in the G1 evidence line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOverlap {
    /// Identifier of the first session (e.g. `"session-20260422-1020"`).
    pub left: String,
    /// Identifier of the second session.
    pub right: String,
}

// ---------------------------------------------------------------------------
// Individual test results
// ---------------------------------------------------------------------------

/// Result of a single test (T1..T7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    /// Which test this result is for.
    pub id: TestId,
    /// The verdict.
    pub verdict: Verdict,
    /// Short, human-readable explanation. Must cite concrete
    /// `.cosmon/` paths or the name of a missing expected path.
    pub evidence: String,
}

/// Result of a single guarantee (G1..G3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuaranteeResult {
    /// Which guarantee.
    pub id: GuaranteeId,
    /// The verdict.
    pub verdict: Verdict,
    /// Short, human-readable explanation.
    pub evidence: String,
}

// ---------------------------------------------------------------------------
// Excluded-substrate patterns
// ---------------------------------------------------------------------------

/// The five excluded-substrate patterns.
///
/// Detection is advisory: the report surfaces the pattern and lets the
/// invitation ceremony decide what to do. Never blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExcludedSubstratePattern {
    /// Anonymous LLM with no persistent `nucleon_id`. Fails T2, G1.
    AnonymousLlmNoId,
    /// Deliberation-of-deliberation without a declared depth bound
    /// (Rice's theorem / halting reduction).
    UnboundedMetacognition,
    /// Substrate with non-append-only memory (fails T1 by construction).
    NonAppendOnlyMemory,
    /// LLM swarm without a designated speaker (G2 unverifiable).
    SwarmWithoutSpeaker,
    /// Noogram-self — admissible in principle, flagged as such.
    NoogramSelf,
}

impl ExcludedSubstratePattern {
    /// Short label for rendering.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AnonymousLlmNoId => "anonymous-llm-no-id",
            Self::UnboundedMetacognition => "unbounded-metacognition",
            Self::NonAppendOnlyMemory => "non-append-only-memory",
            Self::SwarmWithoutSpeaker => "swarm-without-speaker",
            Self::NoogramSelf => "noogram-self",
        }
    }
}

impl fmt::Display for ExcludedSubstratePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// Probe output
// ---------------------------------------------------------------------------

/// Aggregate of the probe step.
///
/// Produced by [`probe`] as a pure function of [`NucleonScan`]. The
/// report step renders this into markdown and derives the admission
/// decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutput {
    /// Per-test results, T1..T7 in order.
    pub tests: Vec<TestResult>,
    /// Per-guarantee results, G1..G3 in order.
    pub guarantees: Vec<GuaranteeResult>,
    /// Any excluded-substrate patterns detected on the candidate.
    pub excluded_patterns: Vec<ExcludedSubstratePattern>,
}

impl ProbeOutput {
    /// Passing-test count out of seven (`k / 7` in the report).
    #[must_use]
    pub fn passing_count(&self) -> u32 {
        u32::try_from(
            self.tests
                .iter()
                .filter(|r| r.verdict == Verdict::Pass)
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Load-bearing subset verdicts, indexed by [`TestId`].
    #[must_use]
    pub fn load_bearing_verdicts(&self) -> BTreeMap<TestId, Verdict> {
        let lb: BTreeSet<TestId> = TestId::load_bearing().into_iter().collect();
        self.tests
            .iter()
            .filter(|r| lb.contains(&r.id))
            .map(|r| (r.id, r.verdict))
            .collect()
    }

    /// Which guarantees are violated (verdict = `Fail`).
    #[must_use]
    pub fn violated_guarantees(&self) -> Vec<GuaranteeId> {
        self.guarantees
            .iter()
            .filter(|r| r.verdict == Verdict::Fail)
            .map(|r| r.id)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Admission decision
// ---------------------------------------------------------------------------

/// The advisory admission decision produced by the report step.
///
/// The decision is *advisory only* — the formula never enforces it.
/// Admission at runtime is the social ceremony of the invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdmissionDecision {
    /// All load-bearing tests pass AND no guarantees violated.
    Admitted,
    /// One or more load-bearing tests fail (or are inconclusive) or
    /// one or more guarantees are violated. The report must name the
    /// smallest fix that would flip the decision to `ADMITTED`.
    Deferred,
    /// Structural incompatibility with cosmon (e.g. substrate cannot
    /// materialize before DAG write). Report lists the architectural
    /// violations that would require a new ADR.
    Refused,
}

impl AdmissionDecision {
    /// Uppercase label for the report (`ADMITTED` / `DEFERRED` / `REFUSED`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Admitted => "ADMITTED",
            Self::Deferred => "DEFERRED",
            Self::Refused => "REFUSED",
        }
    }
}

impl fmt::Display for AdmissionDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// Nucleon report
// ---------------------------------------------------------------------------

/// The canonical admissibility report — pure projection of
/// `(NucleonScan, ProbeOutput)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NucleonReport {
    /// The candidate under audit.
    pub candidate: String,
    /// Optional substrate classification.
    pub substrate: Option<String>,
    /// Scan window in days (0 = all history).
    pub window_days: u32,
    /// Per-test and per-guarantee verdicts.
    pub probe: ProbeOutput,
    /// Advisory admission decision.
    pub decision: AdmissionDecision,
    /// If `Deferred`, the concrete file / schema changes that would
    /// flip the decision to `Admitted`. Empty for `Admitted` /
    /// `Refused`.
    pub smallest_fix: Vec<String>,
}

impl NucleonReport {
    /// Build a report from a scan and its probe output.
    #[must_use]
    pub fn from_probe(scan: &NucleonScan, probe: ProbeOutput) -> Self {
        let decision = decide_admission(&probe);
        let smallest_fix = if decision == AdmissionDecision::Deferred {
            smallest_fix_hints(&probe)
        } else {
            Vec::new()
        };
        NucleonReport {
            candidate: scan.candidate.clone(),
            substrate: scan.substrate.clone(),
            window_days: scan.window_days,
            probe,
            decision,
            smallest_fix,
        }
    }

    /// Render the report as markdown. Stable format — the wire format
    /// is part of the public surface because downstream tooling
    /// (Skylight, the invitation ceremony UI) parses it back.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Nucleon admissibility report\n\n");
        let _ = writeln!(out, "- **Candidate:** `{}`", self.candidate);
        if let Some(substrate) = &self.substrate {
            let _ = writeln!(out, "- **Substrate:** `{substrate}`");
        }
        let window = if self.window_days == 0 {
            "all history".to_string()
        } else {
            format!("{} days", self.window_days)
        };
        let _ = writeln!(out, "- **Window:** {window}");
        let _ = writeln!(
            out,
            "- **Score:** {} / 7 passing",
            self.probe.passing_count()
        );
        let _ = writeln!(
            out,
            "- **Decision:** `{}` (advisory — never blocks)\n",
            self.decision.label()
        );

        out.push_str("## Per-test verdicts\n\n");
        out.push_str("| Test | Verdict | Evidence |\n");
        out.push_str("|------|---------|----------|\n");
        for r in &self.probe.tests {
            let marker = if r.id.is_load_bearing() {
                " **(LB)**"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "| {}{} | {} | {} |",
                r.id.label(),
                marker,
                r.verdict.glyph(),
                escape_pipe(&r.evidence)
            );
        }
        out.push_str(
            "\n_Load-bearing subset: {T1, T2, T4, T5, T6}. All must ✓ for `ADMITTED`._\n\n",
        );

        out.push_str("## Per-guarantee verdicts\n\n");
        out.push_str("| Guarantee | Verdict | Evidence |\n");
        out.push_str("|-----------|---------|----------|\n");
        for r in &self.probe.guarantees {
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                r.id.label(),
                r.verdict.glyph(),
                escape_pipe(&r.evidence)
            );
        }
        out.push('\n');

        if !self.probe.excluded_patterns.is_empty() {
            out.push_str("## Excluded-substrate flags\n\n");
            for p in &self.probe.excluded_patterns {
                let _ = writeln!(out, "- `{}`", p.label());
            }
            out.push('\n');
        }

        if !self.smallest_fix.is_empty() {
            out.push_str("## Smallest fix\n\n");
            for step in &self.smallest_fix {
                let _ = writeln!(out, "- {step}");
            }
            out.push('\n');
        }

        out.push_str(
            "_Telescope, not gate — this report is evidence for the invitation ceremony. \
             The formula never collapses, tackles, or modifies another molecule._\n",
        );
        out
    }
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

// ---------------------------------------------------------------------------
// Probe — pure evaluation of T1..T7 and G1..G3 over a scan
// ---------------------------------------------------------------------------

/// Evaluate T1..T7 and G1..G3 against the scan inventory, returning a
/// [`ProbeOutput`] consumable by [`NucleonReport::from_probe`].
///
/// This is a pure function: no I/O, no clock, no randomness. Feed it a
/// [`NucleonScan`] and the verdicts are determined.
#[must_use]
pub fn probe(scan: &NucleonScan) -> ProbeOutput {
    ProbeOutput {
        tests: TestId::all()
            .iter()
            .map(|&id| probe_test(id, scan))
            .collect(),
        guarantees: GuaranteeId::all()
            .iter()
            .map(|&id| probe_guarantee(id, scan))
            .collect(),
        excluded_patterns: detect_excluded_substrates(scan),
    }
}

#[allow(clippy::too_many_lines)]
fn probe_test(id: TestId, s: &NucleonScan) -> TestResult {
    let (verdict, evidence) = match id {
        TestId::T1 => {
            if s.pilot_sessions == 0 && s.authored_molecules == 0 {
                (
                    Verdict::Inconclusive,
                    "no pilot-sessions or authored molecules yet; causal-trace invariant cannot be evaluated"
                        .to_string(),
                )
            } else if s.carnets_append_only_sealed {
                (
                    Verdict::Pass,
                    format!(
                        "{} pilot-session(s) under .cosmon/state/sessions/; all append-only and sealed",
                        s.pilot_sessions
                    ),
                )
            } else {
                (
                    Verdict::Fail,
                    "at least one session carnet is not append-only or not sealed".to_string(),
                )
            }
        }
        TestId::T2 => {
            if s.observed_ids.is_empty() {
                (
                    Verdict::Inconclusive,
                    "no observed nucleon_id spellings yet".to_string(),
                )
            } else if s.observed_ids.len() == 1 && s.observed_ids.contains(&s.candidate) {
                if s.carnets_append_only_sealed {
                    (
                        Verdict::Pass,
                        format!(
                            "single stable id `{}`; carnets append-only and sealed",
                            s.candidate
                        ),
                    )
                } else {
                    (
                        Verdict::Fail,
                        "id is stable but carnets are not append-only + sealed".to_string(),
                    )
                }
            } else {
                let other: Vec<_> = s
                    .observed_ids
                    .iter()
                    .filter(|i| i.as_str() != s.candidate.as_str())
                    .cloned()
                    .collect();
                (
                    Verdict::Fail,
                    format!("silent id rotation — also observed: {}", other.join(", ")),
                )
            }
        }
        TestId::T3 => {
            if s.authored_molecules == 0 {
                (
                    Verdict::Inconclusive,
                    "no authored molecules yet; SparkedBy traceability cannot be evaluated"
                        .to_string(),
                )
            } else if s.sparked_by_complete {
                (
                    Verdict::Pass,
                    format!(
                        "all {} authored molecules carry a SparkedBy edge back to a session",
                        s.authored_molecules
                    ),
                )
            } else {
                (
                    Verdict::Fail,
                    "at least one authored molecule is orphan (no SparkedBy edge)".to_string(),
                )
            }
        }
        TestId::T4 => {
            let id_ok = s.identity_file_present && s.identity_file_sealed;
            let prose_ok = s.prose_readable;
            match (id_ok, prose_ok) {
                (true, true) => (
                    Verdict::Pass,
                    format!(
                        ".cosmon/state/nucleons/{}/identity.toml present and sealed; carnets are human-readable",
                        s.candidate
                    ),
                ),
                (false, _) => (
                    Verdict::Fail,
                    format!(
                        "expected at .cosmon/state/nucleons/{}/identity.toml; file missing or unsealed",
                        s.candidate
                    ),
                ),
                (true, false) => (
                    Verdict::Fail,
                    "identity file present but carnet is not human-readable prose".to_string(),
                ),
            }
        }
        TestId::T5 => {
            if s.authored_molecules == 0 && s.carnet_entries_total == 0 {
                (
                    Verdict::Inconclusive,
                    "no sparks yet; decidable-authorship invariant cannot be evaluated".to_string(),
                )
            } else if s.duplicate_nucleations > 0 {
                (
                    Verdict::Fail,
                    format!(
                        "{} duplicate nucleations observed in-session — idempotency broken",
                        s.duplicate_nucleations
                    ),
                )
            } else if s.carnet_entries_with_cause == s.carnet_entries_total {
                (
                    Verdict::Pass,
                    format!(
                        "all {} carnet entries carry a `cause:` subline; no duplicate nucleations",
                        s.carnet_entries_total
                    ),
                )
            } else {
                (
                    Verdict::Fail,
                    format!(
                        "{} / {} carnet entries carry a `cause:` subline — authorship not decidable",
                        s.carnet_entries_with_cause, s.carnet_entries_total
                    ),
                )
            }
        }
        TestId::T6 => {
            if s.peer_corruption_detected {
                (
                    Verdict::Fail,
                    "writes into another nucleon's state tree detected".to_string(),
                )
            } else if s.requires_admission_boundary && !s.admission_boundary_present {
                (
                    Verdict::Fail,
                    "non-CLI substrate requires a `<substrate>_event_to_spark` boundary; none present"
                        .to_string(),
                )
            } else if s.requires_admission_boundary {
                (
                    Verdict::Pass,
                    "admission boundary present; no peer corruption observed".to_string(),
                )
            } else {
                (
                    Verdict::Pass,
                    "CLI-native substrate; no peer corruption observed".to_string(),
                )
            }
        }
        TestId::T7 => {
            if !s.reference_human_carnet_present {
                (
                    Verdict::Inconclusive,
                    "no reference human carnet for adversarial comparison — T7 cannot be evaluated"
                        .to_string(),
                )
            } else if s.candidate_distinguishable_from_human {
                (
                    Verdict::Pass,
                    "third-party verifier can distinguish candidate's and reference carnets from .cosmon/ alone"
                        .to_string(),
                )
            } else {
                (
                    Verdict::Fail,
                    "candidate carnet indistinguishable from the reference — evaluability broken"
                        .to_string(),
                )
            }
        }
    };
    TestResult {
        id,
        verdict,
        evidence,
    }
}

fn probe_guarantee(id: GuaranteeId, s: &NucleonScan) -> GuaranteeResult {
    let (verdict, evidence) = match id {
        GuaranteeId::G1 => {
            if s.pilot_sessions == 0 {
                (
                    Verdict::Inconclusive,
                    "no pilot-sessions yet; continuity cannot be evaluated".to_string(),
                )
            } else if s.session_overlaps.is_empty() {
                (
                    Verdict::Pass,
                    format!(
                        "no wall-clock overlap among {} pilot-session(s)",
                        s.pilot_sessions
                    ),
                )
            } else {
                let pairs: Vec<String> = s
                    .session_overlaps
                    .iter()
                    .map(|o| format!("({},{})", o.left, o.right))
                    .collect();
                (
                    Verdict::Fail,
                    format!(
                        "{} overlapping session(s): {}",
                        s.session_overlaps.len(),
                        pairs.join(", ")
                    ),
                )
            }
        }
        GuaranteeId::G2 => {
            if s.authored_molecules == 0 {
                (
                    Verdict::Inconclusive,
                    "no authored molecules yet; non-retroactivity cannot be evaluated".to_string(),
                )
            } else if s.cross_ancestry_writes == 0 {
                (
                    Verdict::Pass,
                    "no writes into molecules outside declared ancestry".to_string(),
                )
            } else {
                (
                    Verdict::Fail,
                    format!(
                        "{} event(s) mutated molecules outside declared ancestry",
                        s.cross_ancestry_writes
                    ),
                )
            }
        }
        GuaranteeId::G3 => {
            if s.sparked_edges == 0 {
                (
                    Verdict::Inconclusive,
                    "no SparkedBy edges yet; legibility cannot be evaluated".to_string(),
                )
            } else if s.illegible_edges == 0 {
                (
                    Verdict::Pass,
                    format!(
                        "cognitive context reconstructable for all {} SparkedBy edge(s)",
                        s.sparked_edges
                    ),
                )
            } else {
                (
                    Verdict::Fail,
                    format!(
                        "{} / {} SparkedBy edge(s) lack reconstructable cognitive context",
                        s.illegible_edges, s.sparked_edges
                    ),
                )
            }
        }
    };
    GuaranteeResult {
        id,
        verdict,
        evidence,
    }
}

/// Detect the advisory excluded-substrate patterns.
#[must_use]
pub fn detect_excluded_substrates(s: &NucleonScan) -> Vec<ExcludedSubstratePattern> {
    let mut out = Vec::new();
    let candidate_lc = s.candidate.to_ascii_lowercase();
    let substrate_lc = s.substrate.as_deref().unwrap_or("").to_ascii_lowercase();
    if s.candidate.trim().is_empty()
        || candidate_lc.starts_with("anon-")
        || candidate_lc == "anonymous"
    {
        out.push(ExcludedSubstratePattern::AnonymousLlmNoId);
    }
    if s.unbounded_metacognition {
        out.push(ExcludedSubstratePattern::UnboundedMetacognition);
    }
    if !s.carnets_append_only_sealed && s.carnet_entries_total > 0 {
        out.push(ExcludedSubstratePattern::NonAppendOnlyMemory);
    }
    if substrate_lc.contains("swarm") && !substrate_lc.contains("speaker") {
        out.push(ExcludedSubstratePattern::SwarmWithoutSpeaker);
    }
    if candidate_lc.contains("noogram") || substrate_lc.contains("noogram") {
        out.push(ExcludedSubstratePattern::NoogramSelf);
    }
    out
}

// ---------------------------------------------------------------------------
// Admission decision — pure function of ProbeOutput
// ---------------------------------------------------------------------------

/// Derive the advisory admission decision from the probe output.
///
/// - `Admitted` ⇔ every load-bearing test passes AND no guarantee is
///   violated.
/// - `Refused` ⇔ any structural incompatibility surfaced by the
///   `non-append-only memory` excluded pattern with `Fail` on T1.
/// - `Deferred` otherwise.
#[must_use]
pub fn decide_admission(probe: &ProbeOutput) -> AdmissionDecision {
    let lb = probe.load_bearing_verdicts();
    let all_lb_pass = TestId::load_bearing()
        .iter()
        .all(|id| lb.get(id).copied() == Some(Verdict::Pass));
    let any_guarantee_fail = probe.guarantees.iter().any(|r| r.verdict == Verdict::Fail);

    let t1_fail = probe
        .tests
        .iter()
        .any(|r| r.id == TestId::T1 && r.verdict == Verdict::Fail);
    let structural = probe
        .excluded_patterns
        .iter()
        .any(|p| matches!(p, ExcludedSubstratePattern::NonAppendOnlyMemory));
    if t1_fail && structural {
        return AdmissionDecision::Refused;
    }

    if all_lb_pass && !any_guarantee_fail {
        AdmissionDecision::Admitted
    } else {
        AdmissionDecision::Deferred
    }
}

/// Emit concrete next-step hints for a `Deferred` decision.
#[must_use]
pub fn smallest_fix_hints(probe: &ProbeOutput) -> Vec<String> {
    let mut hints = Vec::new();
    for r in &probe.tests {
        if !r.id.is_load_bearing() {
            continue;
        }
        match (r.id, r.verdict) {
            (TestId::T1, Verdict::Fail) => hints.push(
                "seal every session carnet append-only (BLAKE3 per-entry chain) before enabling nucleation"
                    .to_string(),
            ),
            (TestId::T2, Verdict::Fail) => hints.push(
                "stabilise the nucleon_id — write a single identity.toml and stop rotating".to_string(),
            ),
            (TestId::T4, Verdict::Fail) => hints.push(
                "add `.cosmon/state/nucleons/<candidate>/identity.toml` (sealed) and ensure carnets are prose"
                    .to_string(),
            ),
            (TestId::T5, Verdict::Fail) => hints.push(
                "add `cause: {kind, agent, channel}` subline on every carnet entry (schema change from ADR-066 §(3))"
                    .to_string(),
            ),
            (TestId::T6, Verdict::Fail) => hints.push(
                "implement `<substrate>_event_to_spark` admission boundary (§8j pattern) for non-CLI substrates"
                    .to_string(),
            ),
            (
                TestId::T1 | TestId::T2 | TestId::T4 | TestId::T5 | TestId::T6,
                Verdict::Inconclusive,
            ) => hints.push(format!(
                "seed the corpus so {} can be evaluated (inconclusive today)",
                r.id.label()
            )),
            _ => {}
        }
    }
    for g in probe.violated_guarantees() {
        hints.push(match g {
            GuaranteeId::G1 => {
                "split or reorder sessions so none overlap in wall-clock time".to_string()
            }
            GuaranteeId::G2 => {
                "route cross-ancestry writes through a typed ingress boundary".to_string()
            }
            GuaranteeId::G3 => {
                "capture cognitive context (prompt.md + journal entries) for every SparkedBy edge"
                    .to_string()
            }
        });
    }
    hints
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn known_good_scan(candidate: &str) -> NucleonScan {
        let mut observed = BTreeSet::new();
        observed.insert(candidate.to_string());
        NucleonScan {
            candidate: candidate.to_string(),
            substrate: Some("human-operator".to_string()),
            window_days: 30,
            pilot_sessions: 4,
            authored_molecules: 12,
            sparked_edges: 12,
            carnet_entries_with_cause: 30,
            carnet_entries_total: 30,
            identity_file_present: true,
            identity_file_sealed: true,
            carnets_append_only_sealed: true,
            observed_ids: observed,
            sparked_by_complete: true,
            peer_corruption_detected: false,
            prose_readable: true,
            requires_admission_boundary: false,
            admission_boundary_present: false,
            session_overlaps: Vec::new(),
            cross_ancestry_writes: 0,
            illegible_edges: 0,
            duplicate_nucleations: 0,
            unbounded_metacognition: false,
            reference_human_carnet_present: true,
            candidate_distinguishable_from_human: true,
        }
    }

    #[test]
    fn test_all_test_ids_have_distinct_labels() {
        let labels: BTreeSet<&str> = TestId::all().iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), 7);
    }

    #[test]
    fn test_load_bearing_subset_is_exactly_five() {
        let lb = TestId::load_bearing();
        assert_eq!(lb.len(), 5);
        assert!(lb.contains(&TestId::T1));
        assert!(lb.contains(&TestId::T2));
        assert!(lb.contains(&TestId::T4));
        assert!(lb.contains(&TestId::T5));
        assert!(lb.contains(&TestId::T6));
        assert!(!lb.contains(&TestId::T3));
        assert!(!lb.contains(&TestId::T7));
    }

    #[test]
    fn test_known_good_candidate_passes_all_seven() {
        let scan = known_good_scan("you");
        let probe_out = probe(&scan);
        assert_eq!(
            probe_out.passing_count(),
            7,
            "expected 7/7 pass on known-good scan, got {}/7: {:?}",
            probe_out.passing_count(),
            probe_out.tests
        );
        for g in &probe_out.guarantees {
            assert_eq!(g.verdict, Verdict::Pass, "guarantee {} not pass", g.id);
        }
        assert_eq!(decide_admission(&probe_out), AdmissionDecision::Admitted);
    }

    #[test]
    fn test_t2_failure_on_rotated_nucleon_id_is_named_in_report() {
        let mut scan = known_good_scan("you");
        // Synthetic rotation: the session carnet reveals a second id.
        scan.observed_ids.insert("you-v2".to_string());

        let probe_out = probe(&scan);
        let t2 = probe_out
            .tests
            .iter()
            .find(|r| r.id == TestId::T2)
            .expect("T2 missing");
        assert_eq!(t2.verdict, Verdict::Fail);
        assert!(
            t2.evidence.contains("you-v2"),
            "T2 evidence must name the rotated id, got: {}",
            t2.evidence
        );

        let report = NucleonReport::from_probe(&scan, probe_out);
        assert_eq!(report.decision, AdmissionDecision::Deferred);
        let md = report.render_markdown();
        assert!(
            md.contains("you-v2"),
            "rendered report must name the rotated id"
        );
        assert!(
            md.contains("DEFERRED"),
            "rendered report must show DEFERRED"
        );
        assert!(
            md.contains("stabilise the nucleon_id"),
            "smallest-fix must suggest id stabilisation"
        );
    }

    #[test]
    fn test_inconclusive_when_corpus_empty() {
        let scan = NucleonScan {
            candidate: "fresh".to_string(),
            ..NucleonScan::default()
        };
        let probe_out = probe(&scan);
        let t3 = probe_out.tests.iter().find(|r| r.id == TestId::T3).unwrap();
        assert_eq!(t3.verdict, Verdict::Inconclusive);
    }

    #[test]
    fn test_excluded_substrate_anonymous_llm() {
        let scan = NucleonScan {
            candidate: "anon-123".to_string(),
            ..NucleonScan::default()
        };
        let patterns = detect_excluded_substrates(&scan);
        assert!(patterns.contains(&ExcludedSubstratePattern::AnonymousLlmNoId));
    }

    #[test]
    fn test_refused_on_non_append_only_plus_t1_fail() {
        let mut scan = known_good_scan("you");
        scan.carnets_append_only_sealed = false;
        let probe_out = probe(&scan);
        assert_eq!(decide_admission(&probe_out), AdmissionDecision::Refused);
    }

    #[test]
    fn test_rendered_markdown_is_stable_enough_to_parse() {
        let scan = known_good_scan("you");
        let report = NucleonReport::from_probe(&scan, probe(&scan));
        let md = report.render_markdown();
        assert!(md.starts_with("# Nucleon admissibility report"));
        assert!(md.contains("| T1"));
        assert!(md.contains("| G1"));
        assert!(md.contains("ADMITTED"));
    }
}
