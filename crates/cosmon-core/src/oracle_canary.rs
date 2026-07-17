// SPDX-License-Identifier: AGPL-3.0-only

//! Oracle canary — the multi-provider access **smoke alarm**.
//!
//! # Why this module exists
//!
//! When a major LLM vendor became export-restricted on verbal, same-day
//! notice — no 90-day wind-down — the load-bearing boundary condition was
//! that the **reaction-time budget is zero**. A plan-B that takes two weeks
//! to stand up is, against this boundary condition, equivalent to having no
//! plan-B.
//!
//! You cannot *decide the future order* — predicting whether/when Commerce
//! issues one is undecidable against an adaptive authority. But you **can**
//! continuously *decide the present access state* — exactly the observable that
//! flips when an order lands. That is what this module's decision core consumes:
//! per-provider probe readings, and a diff that converts a silent global
//! cutoff into a paged alert at t=0.
//!
//! **Scope honesty (turing).** This is a *smoke alarm, not a weather forecast*.
//! It detects an order at t=0 and buys minutes-to-hours of failover head-start;
//! it does **not** predict whether/when an order will issue. Do not read
//! prediction into it.
//!
//! # The three bits
//!
//! Every probe records three independent bits per provider
//! ([`AccessBits`]):
//!
//! - **reachable** — the auth/API endpoint responds at all.
//! - **capable** — output is well-formed; a long-context needle is retrieved;
//!   a structured-`--json` assertion holds.
//! - **policy-open** — no new geo/nationality refusal signature in the
//!   response.
//!
//! A flip of `reachable → false` or `policy_open → true → false` **on a
//! critical provider** (Claude) is the export-order smoke alarm → page the
//! operator. The same probe run against the standby providers
//! (Mistral / local) **doubles as the weekly warm-standby check**: a standby
//! you never fire is cold the day you need it, so its flips surface as health
//! notes rather than pages.
//!
//! # Zero I/O
//!
//! Like [`crate::model_chain`], this module is pure. The actual network probe
//! is injected by the caller (the `oracle-canary` formula's worker, or a future
//! scheduler integration); the core here only *decides* from the bits the probe
//! produced. This is the executable spec of the alarm, unit-testable without a
//! live endpoint.

use serde::{Deserialize, Serialize};

/// The canonical name of the **critical** provider whose flip pages the
/// operator at t=0 — the one the export order targets.
///
/// A flip on this provider is the export-order alarm. Flips on any other
/// (standby) provider are warm-standby health notes, not pages. Kept as a
/// single literal so the "which provider is load-bearing" decision lives in
/// exactly one place.
pub const CRITICAL_PROVIDER: &str = "claude";

/// The three independent access bits recorded for one provider at one instant.
///
/// Each bit answers a strictly narrower question than the last: an endpoint can
/// be *reachable* but not *capable* (auth works, output is garbage), and
/// *capable* but not *policy-open* (it answers fine, then refuses on a
/// geo/nationality signature). The triple is the minimal observable that
/// distinguishes "the lock just closed" from "the network blipped".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessBits {
    /// The auth/API endpoint responds at all.
    pub reachable: bool,
    /// Output is well-formed and passes the capability assertion
    /// (needle retrieved, `--json` shape holds).
    pub capable: bool,
    /// No new geo/nationality refusal signature in the response.
    pub policy_open: bool,
}

impl AccessBits {
    /// All three bits set — the provider is fully usable.
    pub const OPEN: AccessBits = AccessBits {
        reachable: true,
        capable: true,
        policy_open: true,
    };

    /// Collapse the three bits into a single coarse [`AccessState`] verdict for
    /// reporting. The order of checks matters: unreachable dominates (you learn
    /// nothing about capability or policy from an endpoint that never answered),
    /// then policy, then capability.
    #[must_use]
    pub fn state(self) -> AccessState {
        if !self.reachable {
            AccessState::Down
        } else if !self.policy_open {
            AccessState::Blocked
        } else if !self.capable {
            AccessState::Degraded
        } else {
            AccessState::Open
        }
    }
}

/// The coarse, human-facing verdict for one provider, derived from
/// [`AccessBits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessState {
    /// Reachable, capable, policy-open — fully usable.
    Open,
    /// Reachable and policy-open, but output failed the capability assertion.
    Degraded,
    /// Reachable but a policy/geo refusal signature is present — the
    /// export-order shape.
    Blocked,
    /// The endpoint did not respond at all.
    Down,
}

/// One provider's probe result at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReading {
    /// The provider name as it appears in `.cosmon/config.toml` `[adapters]`
    /// (e.g. `claude`, `mistral`, `local`).
    pub provider: String,
    /// The three access bits.
    pub bits: AccessBits,
    /// Free-form detail captured by the probe (HTTP status, refusal signature
    /// excerpt, latency) — for the audit trail, not for the decision.
    pub detail: String,
}

impl ProviderReading {
    /// Construct a reading.
    pub fn new(provider: impl Into<String>, bits: AccessBits, detail: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            bits,
            detail: detail.into(),
        }
    }
}

/// A full canary sweep across every wired provider at one instant.
///
/// This is the durable artifact the `oracle-canary` molecule persists to its
/// state directory as JSON evidence; the *next* sweep diffs against it via
/// [`flip_alerts`] to decide whether to page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryReading {
    /// One reading per probed provider.
    pub readings: Vec<ProviderReading>,
}

impl CanaryReading {
    /// Build a reading from a list of per-provider readings.
    #[must_use]
    pub fn new(readings: Vec<ProviderReading>) -> Self {
        Self { readings }
    }

    /// The bits recorded for `provider` in this sweep, if it was probed.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<&ProviderReading> {
        self.readings.iter().find(|r| r.provider == provider)
    }

    /// `true` when no provider was probed — the empty baseline, against which
    /// no flip can be computed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.readings.is_empty()
    }
}

/// The kind of transition a provider underwent between two sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlipKind {
    /// `reachable` went `true → false` — the endpoint stopped answering.
    WentUnreachable,
    /// `policy_open` went `true → false` — a new geo/policy refusal appeared.
    /// This is the export-order signature.
    PolicyBlocked,
    /// `capable` went `true → false` while still reachable and policy-open —
    /// degraded output (a softer signal than the two above).
    Degraded,
    /// The provider moved back toward [`AccessState::Open`] on some bit — a
    /// recovery, informational only.
    Recovered,
}

/// How loudly a flip should be surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Page the operator now (t=0). A critical-provider cutoff: execute the
    /// rehearsed cutover.
    Page,
    /// A standby provider's health changed — note it (the weekly warm-standby
    /// check), do not page.
    StandbyNote,
    /// A provider recovered — informational, never pages.
    Recovery,
}

/// One detected transition between two sweeps, with the paging verdict applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryAlert {
    /// The provider that flipped.
    pub provider: String,
    /// What kind of transition it was.
    pub kind: FlipKind,
    /// How loudly to surface it.
    pub severity: Severity,
    /// The coarse state before the flip.
    pub from: AccessState,
    /// The coarse state after the flip.
    pub to: AccessState,
    /// A one-line, operator-facing message (Feynman register).
    pub message: String,
}

/// Decide whether a flip on `provider` of the given degradation `kind` is a
/// page or a standby note.
///
/// The single rule that makes the canary a *smoke alarm and not a chime*: only
/// a degradation (`WentUnreachable` / `PolicyBlocked`) on a **critical**
/// provider pages. Everything else — standby degradation, any recovery — is a
/// note. `critical` is supplied by the caller (default: name equals
/// [`CRITICAL_PROVIDER`]).
fn severity_for(kind: FlipKind, critical: bool) -> Severity {
    match kind {
        FlipKind::Recovered => Severity::Recovery,
        FlipKind::WentUnreachable | FlipKind::PolicyBlocked | FlipKind::Degraded => {
            if critical {
                Severity::Page
            } else {
                Severity::StandbyNote
            }
        }
    }
}

/// Classify the single most significant transition between two sets of bits.
///
/// Returns `None` when nothing material changed. When several bits moved at
/// once, the *worst* degradation wins (unreachable > policy-block > degraded),
/// because that is the one the operator must act on; a simultaneous recovery on
/// a lesser bit is irrelevant while a worse bit just failed. A pure improvement
/// is reported as [`FlipKind::Recovered`].
fn classify(prev: AccessBits, curr: AccessBits) -> Option<FlipKind> {
    // Degradations, worst first.
    if prev.reachable && !curr.reachable {
        return Some(FlipKind::WentUnreachable);
    }
    if prev.policy_open && !curr.policy_open {
        return Some(FlipKind::PolicyBlocked);
    }
    if prev.capable && !curr.capable {
        return Some(FlipKind::Degraded);
    }
    // No degradation — did anything improve?
    let improved = (!prev.reachable && curr.reachable)
        || (!prev.policy_open && curr.policy_open)
        || (!prev.capable && curr.capable);
    if improved {
        Some(FlipKind::Recovered)
    } else {
        None
    }
}

/// Diff two sweeps and emit one [`CanaryAlert`] per provider that changed
/// state, with the paging verdict applied.
///
/// `critical_providers` names the providers whose degradation pages the
/// operator at t=0 (typically just [`CRITICAL_PROVIDER`]); a degradation on any
/// other provider is a warm-standby note.
///
/// Semantics:
/// - A provider present in `curr` but absent from `prev` produces **no** alert
///   — it is a new baseline, not a flip (this also makes the very first sweep,
///   against an empty `prev`, silent by construction).
/// - A provider that vanished from `curr` produces no alert — the probe simply
///   did not run it this sweep; absence of a reading is not a flip.
/// - Only material bit changes (`classify`) produce alerts.
///
/// The result is ordered to match `curr.readings`, so the report reads in the
/// same provider order the probe emitted.
#[must_use]
pub fn flip_alerts(
    prev: &CanaryReading,
    curr: &CanaryReading,
    critical_providers: &[&str],
) -> Vec<CanaryAlert> {
    let mut alerts = Vec::new();
    for reading in &curr.readings {
        let Some(before) = prev.get(&reading.provider) else {
            continue; // new provider → baseline, not a flip
        };
        let Some(kind) = classify(before.bits, reading.bits) else {
            continue; // nothing material changed
        };
        let critical = critical_providers.contains(&reading.provider.as_str());
        let severity = severity_for(kind, critical);
        alerts.push(CanaryAlert {
            provider: reading.provider.clone(),
            kind,
            severity,
            from: before.bits.state(),
            to: reading.bits.state(),
            message: render_message(&reading.provider, kind, severity),
        });
    }
    alerts
}

/// `true` when at least one alert in `alerts` is a [`Severity::Page`] — i.e. a
/// critical-provider cutoff was detected and the operator must be paged now.
#[must_use]
pub fn needs_paging(alerts: &[CanaryAlert]) -> bool {
    alerts.iter().any(|a| a.severity == Severity::Page)
}

/// Render the one-line operator-facing message for a flip (Feynman register —
/// a picture, not a field).
fn render_message(provider: &str, kind: FlipKind, severity: Severity) -> String {
    match (kind, severity) {
        (FlipKind::WentUnreachable, Severity::Page) => format!(
            "🚨 {provider} just went dark — the lock may have closed. \
             Execute the rehearsed cutover NOW."
        ),
        (FlipKind::PolicyBlocked, Severity::Page) => format!(
            "🚨 {provider} answered with a new policy refusal — export-order \
             signature. Execute the rehearsed cutover NOW."
        ),
        (FlipKind::Degraded, Severity::Page) => format!(
            "⚠️ {provider} is reachable but its output is broken — \
             treat as failing, consider the cutover."
        ),
        (kind, Severity::StandbyNote) => format!(
            "🌡️ standby {provider} health changed ({kind:?}) — the warm path \
             is colder than it should be; check it before you need it."
        ),
        (_, Severity::Recovery) => {
            format!("✅ {provider} recovered — access restored.")
        }
        // Page severity only attaches to the three degradation kinds above; this
        // arm is unreachable in practice but keeps the match total.
        (kind, Severity::Page) => {
            format!("🚨 {provider} flipped ({kind:?}) — investigate.")
        }
    }
}

/// Scan free-form response text for a geo / nationality / export refusal
/// signature, to set the `policy_open` bit.
///
/// This is a *heuristic*, deliberately conservative: it matches the phrase
/// shapes an export-order refusal takes, not a closed list of countries. A
/// match means "treat `policy_open` as false and let the operator judge" — a
/// false positive costs one investigated page, a false negative costs the whole
/// reaction-time budget, so the asymmetry favours matching.
///
/// ```
/// use cosmon_core::oracle_canary::is_policy_block_signature;
///
/// assert!(is_policy_block_signature(
///     "I'm sorry, but this model is not available in your region."
/// ));
/// assert!(is_policy_block_signature(
///     "Access denied due to export control restrictions."
/// ));
/// assert!(!is_policy_block_signature("Here is the answer you asked for."));
/// ```
#[must_use]
pub fn is_policy_block_signature(text: &str) -> bool {
    const SIGNATURES: &[&str] = &[
        "not available in your region",
        "not available in your country",
        "unavailable in your region",
        "unavailable in your country",
        "export control",
        "export restriction",
        "export-control",
        "geographic restriction",
        "geo restriction",
        "restricted in your region",
        "based on your location",
        "due to your location",
        "nationality",
        "sanctioned",
        "embargo",
    ];
    let lower = text.to_lowercase();
    SIGNATURES.iter().any(|sig| lower.contains(sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(reachable: bool, capable: bool, policy_open: bool) -> AccessBits {
        AccessBits {
            reachable,
            capable,
            policy_open,
        }
    }

    fn sweep(rows: &[(&str, AccessBits)]) -> CanaryReading {
        CanaryReading::new(
            rows.iter()
                .map(|(p, b)| ProviderReading::new(*p, *b, ""))
                .collect(),
        )
    }

    #[test]
    // The whole point of this test is to pin the literal shape of the `OPEN`
    // constant: every flag must be `true`. clippy sees compile-time constants
    // and flags `assertions_on_constants`, but asserting the constant here is
    // exactly the spec we want to freeze.
    #[allow(clippy::assertions_on_constants)]
    fn open_constant_is_all_true() {
        const { assert!(AccessBits::OPEN.reachable) };
        const { assert!(AccessBits::OPEN.capable) };
        const { assert!(AccessBits::OPEN.policy_open) };
        assert_eq!(AccessBits::OPEN.state(), AccessState::Open);
    }

    #[test]
    fn state_unreachable_dominates() {
        // Even with the other bits true-by-default, no-reachable is Down.
        assert_eq!(bits(false, true, true).state(), AccessState::Down);
        // Unreachable dominates capability and policy regardless of their value.
        assert_eq!(bits(false, false, false).state(), AccessState::Down);
    }

    #[test]
    fn state_policy_block_ranks_above_capability() {
        // Reachable, capable, but policy-closed → Blocked (the export shape).
        assert_eq!(bits(true, true, false).state(), AccessState::Blocked);
        // Policy outranks a simultaneous capability failure.
        assert_eq!(bits(true, false, false).state(), AccessState::Blocked);
    }

    #[test]
    fn state_degraded_when_only_capability_fails() {
        assert_eq!(bits(true, false, true).state(), AccessState::Degraded);
    }

    #[test]
    fn first_sweep_against_empty_baseline_is_silent() {
        let prev = CanaryReading::default();
        let curr = sweep(&[("claude", AccessBits::OPEN), ("mistral", AccessBits::OPEN)]);
        assert!(prev.is_empty());
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert!(
            alerts.is_empty(),
            "first sweep establishes baseline, no page"
        );
    }

    #[test]
    fn new_provider_is_baseline_not_a_flip() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        // mistral appears for the first time, already down — but it is new, so
        // no flip is reported for it.
        let curr = sweep(&[
            ("claude", AccessBits::OPEN),
            ("mistral", bits(false, false, false)),
        ]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert!(alerts.is_empty());
    }

    #[test]
    fn stable_state_produces_no_alert() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        let curr = sweep(&[("claude", AccessBits::OPEN)]);
        assert!(flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]).is_empty());
    }

    /// The load-bearing gate: Claude going unreachable pages at t=0.
    #[test]
    fn claude_went_unreachable_pages() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        let curr = sweep(&[("claude", bits(false, false, false))]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, FlipKind::WentUnreachable);
        assert_eq!(alerts[0].severity, Severity::Page);
        assert_eq!(alerts[0].from, AccessState::Open);
        assert_eq!(alerts[0].to, AccessState::Down);
        assert!(needs_paging(&alerts));
    }

    /// The other export-order shape: Claude answers with a policy refusal.
    #[test]
    fn claude_policy_block_pages() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        let curr = sweep(&[("claude", bits(true, true, false))]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, FlipKind::PolicyBlocked);
        assert_eq!(alerts[0].severity, Severity::Page);
        assert_eq!(alerts[0].to, AccessState::Blocked);
        assert!(needs_paging(&alerts));
    }

    /// A standby provider flipping is a note, never a page.
    #[test]
    fn standby_flip_is_a_note_not_a_page() {
        let prev = sweep(&[("claude", AccessBits::OPEN), ("mistral", AccessBits::OPEN)]);
        let curr = sweep(&[
            ("claude", AccessBits::OPEN),
            ("mistral", bits(false, false, false)),
        ]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].provider, "mistral");
        assert_eq!(alerts[0].severity, Severity::StandbyNote);
        assert!(!needs_paging(&alerts), "a cold standby must not page");
    }

    #[test]
    fn recovery_is_informational_never_pages() {
        let prev = sweep(&[("claude", bits(false, false, false))]);
        let curr = sweep(&[("claude", AccessBits::OPEN)]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, FlipKind::Recovered);
        assert_eq!(alerts[0].severity, Severity::Recovery);
        assert!(!needs_paging(&alerts));
    }

    /// When several bits degrade at once, the worst one is reported so the
    /// operator acts on the dominant failure.
    #[test]
    fn simultaneous_degradations_report_worst_first() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        // reachable AND policy_open both dropped — unreachable is worse.
        let curr = sweep(&[("claude", bits(false, true, false))]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts[0].kind, FlipKind::WentUnreachable);
    }

    #[test]
    fn multi_provider_sweep_pages_only_on_critical() {
        let prev = sweep(&[
            ("claude", AccessBits::OPEN),
            ("mistral", AccessBits::OPEN),
            ("local", AccessBits::OPEN),
        ]);
        // claude policy-blocked (page), mistral unreachable (note), local fine.
        let curr = sweep(&[
            ("claude", bits(true, true, false)),
            ("mistral", bits(false, false, false)),
            ("local", AccessBits::OPEN),
        ]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        assert_eq!(alerts.len(), 2);
        assert!(needs_paging(&alerts));
        let pages: Vec<&CanaryAlert> = alerts
            .iter()
            .filter(|a| a.severity == Severity::Page)
            .collect();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].provider, "claude");
    }

    #[test]
    fn vanished_provider_is_not_a_flip() {
        // claude was probed last time, not this time (probe skipped it). Its
        // absence is not a flip — we report nothing rather than a false page.
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        let curr = CanaryReading::default();
        assert!(flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]).is_empty());
    }

    #[test]
    fn policy_signature_matches_export_refusals() {
        assert!(is_policy_block_signature(
            "Sorry, this is not available in your region."
        ));
        assert!(is_policy_block_signature(
            "Blocked due to EXPORT CONTROL regulations."
        ));
        assert!(is_policy_block_signature(
            "We cannot serve users based on your location."
        ));
        assert!(!is_policy_block_signature(
            "Here is a perfectly normal completion."
        ));
        assert!(!is_policy_block_signature(""));
    }

    #[test]
    fn reading_roundtrips_through_json() {
        let reading = sweep(&[
            ("claude", AccessBits::OPEN),
            ("mistral", bits(true, true, false)),
        ]);
        let json = serde_json::to_string(&reading).unwrap();
        let back: CanaryReading = serde_json::from_str(&json).unwrap();
        assert_eq!(reading, back);
        assert_eq!(
            back.get("mistral").unwrap().bits.state(),
            AccessState::Blocked
        );
    }

    #[test]
    fn alert_serialises_with_kebab_case_tags() {
        let prev = sweep(&[("claude", AccessBits::OPEN)]);
        let curr = sweep(&[("claude", bits(true, true, false))]);
        let alerts = flip_alerts(&prev, &curr, &[CRITICAL_PROVIDER]);
        let json = serde_json::to_string(&alerts[0]).unwrap();
        assert!(json.contains("\"policy-blocked\""));
        assert!(json.contains("\"page\""));
        assert!(json.contains("\"blocked\""));
    }
}
