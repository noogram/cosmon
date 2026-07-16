// SPDX-License-Identifier: AGPL-3.0-only

//! Token-consumption instrumentation — IFBDD measurement of the
//! `LlmBackend::complete` boundary.
//!
//! Sibling of [`crate::instrumentation`] (the authz-decision NDJSON sink)
//! and of T1's `EngineCallEntered` in `cosmon-api`. Records every LLM
//! call's token accounting per `(tenant, molecule, kind, backend)` so
//! the operator can later answer two empirical questions:
//!
//! 1. **How much does each tenant consume**, by molecule kind and
//!    backend? Three tenants × N molecules × M kinds is the data
//!    needed to compute the Anthropic-vs-MLX cost ratio.
//! 2. **Is the V1 BYOK billing layer warranted?** That decision waits
//!    on real consumption data, not assumed traffic — instrument the
//!    consumption *before* freezing the billing model.
//!
//! # This is observability, not billing
//!
//! `TokenUsage` carries `cost_micros_estimated` as a *measurement*,
//! not a billing fact. The field is informational: a future billing
//! system may look at it for correlation, but no `Billing` module,
//! no `BillingProvider` trait, no invoice flow exists on top of it
//! today. The seal pattern from
//! [`crate::briefing_seal`] applies: trace, not lock.
//!
//! Billing decisions await an empirical IFBDD signal.
//!
//! # Observation, not enforcement
//!
//! The instrumentation never blocks the hot path, never persists into
//! `state.json` (this is system telemetry), and any IO failure is
//! silently swallowed. Single-writer NDJSON file invariant — no RAM
//! aggregation.
//!
//! # Sink
//!
//! One NDJSON sink: `{state_dir}/instrumentation/tokens.jsonl`. The
//! `COSMON_TOKEN_INSTRUMENTATION_PATH` environment variable overrides
//! the path — used by integration tests that point it at a tempfile.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use cosmon_core::id::{MoleculeId, NucleonId};
use cosmon_core::kind::MoleculeKind;

/// One recorded token-usage event.
///
/// Wire-format-stable fields: external scrapers may rely on the JSON
/// shape across V0..V1. New optional fields use
/// `#[serde(skip_serializing_if = "Option::is_none")]` so legacy readers
/// keep working.
///
/// # Field semantics
///
/// - `tenant` — the [`NucleonId`] of the principal who launched the
///   call. For the V0 trusted-operator flow this is `"operator"`; for
///   JWT-bearing remote pilots it is the `sub`-mapped nucleon.
/// - `molecule_id` — the molecule under whose execution the call was
///   issued. The `(tenant, molecule_id)` pair lets the aggregator
///   answer "tokens spent advancing this molecule".
/// - `kind` — molecule kind classification. `None` for legacy
///   molecules; otherwise the kind at call time.
/// - `backend` — free-form backend identifier
///   (`"anthropic"`, `"openai"`, `"ollama"`, `"mlx"`, …). Stringly-typed
///   on purpose: the IFBDD goal is to learn which backend strings show
///   up in practice before crystallising a typed enum.
/// - `tokens_in` / `tokens_out` — input / output token counts as
///   reported by the backend. `0` is a legitimate value (e.g. cached
///   responses).
/// - `cost_micros_estimated` — a *measurement*, computed from
///   `pricing_table_version`. **Not a billing fact** (see module docs).
/// - `pricing_table_version` — opaque tag identifying the pricing
///   table that produced `cost_micros_estimated`. Lets retrospective
///   audits recompute costs at a different table. `None` when the
///   pricing source is unknown — caller responsibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// The principal whose budget is debited (operator or JWT-mapped
    /// nucleon).
    pub tenant: NucleonId,
    /// The molecule under whose execution the call was issued.
    pub molecule_id: MoleculeId,
    /// Molecule kind at call time. `None` for legacy molecules
    /// (kind unset before ADR-013).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MoleculeKind>,
    /// Backend identifier — stringly-typed by design (see field docs).
    pub backend: String,
    /// Input token count reported by the backend.
    pub tokens_in: u64,
    /// Output token count reported by the backend.
    pub tokens_out: u64,
    /// **Measurement, not billing.** Estimated cost in micro-units of
    /// the smallest accounting currency (typically USD micros). Zero
    /// when the caller has no pricing table for this backend.
    pub cost_micros_estimated: u64,
    /// Opaque version tag for the pricing table that computed
    /// `cost_micros_estimated`. Future audits use it to recompute
    /// costs at an alternate table without re-running calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_table_version: Option<String>,
    /// UTC timestamp when the event was emitted.
    pub timestamp: DateTime<Utc>,
}

/// Relative NDJSON path under the cosmon state directory.
pub const TOKEN_NDJSON_RELATIVE_PATH: &str = "instrumentation/tokens.jsonl";

/// Resolve the absolute NDJSON path. The
/// `COSMON_TOKEN_INSTRUMENTATION_PATH` env var wins over the default
/// `{state_dir}/instrumentation/tokens.jsonl` so integration tests can
/// isolate captures.
#[must_use]
pub fn resolve_token_path(state_dir: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("COSMON_TOKEN_INSTRUMENTATION_PATH") {
        return PathBuf::from(p);
    }
    state_dir.join(TOKEN_NDJSON_RELATIVE_PATH)
}

/// Process-wide append lock — keeps two concurrent threads from
/// interleaving partial JSON lines on the same NDJSON file.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Emit one [`TokenUsage`] event to the NDJSON sink.
///
/// Best-effort: the function never panics, never blocks the caller on
/// an I/O error, and never reports a result. A serialise or write
/// failure is silently swallowed in keeping with the seal pattern from
/// [`crate::briefing_seal`].
///
/// # Wire integration
///
/// Today the helper is exposed but **not yet** invoked from
/// `LlmBackend::complete`. T-V1-API-SHAPE will introduce
/// `ResponseMetrics.cost_micros`; once that lands, the backend wrapper
/// calls this helper at end-of-call. Until then the helper is the
/// stable shape future callers depend on (instrumentation pattern
/// already proven by [`crate::instrumentation::emit_authz_decision`]).
#[allow(clippy::too_many_arguments)]
pub fn emit_token_usage(
    state_dir: &Path,
    tenant: &NucleonId,
    molecule_id: &MoleculeId,
    kind: Option<MoleculeKind>,
    backend: &str,
    tokens_in: u64,
    tokens_out: u64,
    cost_micros_estimated: u64,
    pricing_table_version: Option<&str>,
) {
    let event = TokenUsage {
        tenant: tenant.clone(),
        molecule_id: molecule_id.clone(),
        kind,
        backend: backend.to_owned(),
        tokens_in,
        tokens_out,
        cost_micros_estimated,
        pricing_table_version: pricing_table_version.map(str::to_owned),
        timestamp: Utc::now(),
    };

    let path = resolve_token_path(state_dir);
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

/// Read every event from an NDJSON file. Used by `cs tokens` and by
/// integration tests. Returns an empty `Vec` when the file does not
/// exist.
///
/// Malformed lines are silently skipped — V0 instrumentation is
/// best-effort capture, not a contract; a partial line from a crash
/// must not poison the read path.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the file exists but
/// cannot be read (e.g. permissions).
pub fn read_token_ndjson(path: &Path) -> std::io::Result<Vec<TokenUsage>> {
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
        if let Ok(ev) = serde_json::from_str::<TokenUsage>(trimmed) {
            out.push(ev);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Per-molecule aggregation
// ---------------------------------------------------------------------------

/// Sum of API token usage recorded against a single molecule.
///
/// This is the read-side answer to the operator's question *"how many
/// API tokens did advancing this molecule cost?"* — the per-`molecule_id`
/// fold of every [`TokenUsage`] event in the canonical sink. It is the
/// `cs observe` / `GET /v1/molecules/:id` counterpart of the per-tenant
/// rows surfaced by `cs tokens`.
///
/// # Observability, not billing
///
/// `cost_micros_estimated` carries the same caveat as the underlying
/// [`TokenUsage`] field: it is a *measurement* summed across events, not
/// a billing fact. No invoice flow reads it.
///
/// Wire-format-stable: serialised verbatim into the `api_tokens` slot of
/// the molecule projection. New fields must stay additive (the
/// omit-if-none discipline used across the observe envelope).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoleculeTokenTotals {
    /// Sum of `tokens_in` across every recorded call for the molecule.
    pub tokens_in: u64,
    /// Sum of `tokens_out` across every recorded call for the molecule.
    pub tokens_out: u64,
    /// Sum of the per-call estimated cost, in USD micros. **Measurement,
    /// not billing** (see [`TokenUsage::cost_micros_estimated`]).
    pub cost_micros_estimated: u64,
    /// How many LLM calls were recorded against the molecule. A `0` total
    /// is never produced — the aggregator omits molecules with no events.
    pub invocations: u64,
}

impl MoleculeTokenTotals {
    /// Total tokens billed across the molecule's lifetime (`in + out`).
    /// Saturates rather than overflowing on a pathological log.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    /// Fold one more [`TokenUsage`] event into the running total.
    fn add(&mut self, ev: &TokenUsage) {
        self.tokens_in = self.tokens_in.saturating_add(ev.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(ev.tokens_out);
        self.cost_micros_estimated = self
            .cost_micros_estimated
            .saturating_add(ev.cost_micros_estimated);
        self.invocations = self.invocations.saturating_add(1);
    }
}

/// Fold a slice of [`TokenUsage`] events into per-molecule totals.
///
/// Deterministic ordering — the `BTreeMap` keys by [`MoleculeId`] so two
/// runs over the same log produce byte-identical output, which keeps the
/// downstream JSON stable for snapshot tests and external scrapers.
#[must_use]
pub fn aggregate_by_molecule(events: &[TokenUsage]) -> BTreeMap<MoleculeId, MoleculeTokenTotals> {
    let mut by_mol: BTreeMap<MoleculeId, MoleculeTokenTotals> = BTreeMap::new();
    for ev in events {
        by_mol.entry(ev.molecule_id.clone()).or_default().add(ev);
    }
    by_mol
}

/// Sum the API token usage recorded for one molecule from the canonical
/// sink at `{state_dir}/instrumentation/tokens.jsonl`.
///
/// Returns `None` when the sink is absent, unreadable, or holds no event
/// matching `molecule_id`. This is the **omit-if-none** signal the
/// observe projection uses to skip the `api_tokens` field entirely —
/// identical discipline to [`crate::wait::collect_molecule_energy`].
///
/// The read is best-effort and never errors: a missing or corrupt sink
/// must not block a read-only `cs observe`, exactly as the write side
/// ([`emit_token_usage`]) never blocks the hot path.
#[must_use]
pub fn molecule_token_totals(
    state_dir: &Path,
    molecule_id: &MoleculeId,
) -> Option<MoleculeTokenTotals> {
    let path = resolve_token_path(state_dir);
    let events = read_token_ndjson(&path).ok()?;
    let mut totals = MoleculeTokenTotals::default();
    let mut matched = false;
    for ev in &events {
        if &ev.molecule_id == molecule_id {
            totals.add(ev);
            matched = true;
        }
    }
    matched.then_some(totals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Serialise every test in this module — env state is process-global,
    /// so a test that sets `COSMON_TOKEN_INSTRUMENTATION_PATH` would
    /// otherwise race a concurrent test that reads the default path.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn mol(id: &str) -> MoleculeId {
        MoleculeId::new(id).unwrap()
    }

    fn tenant(id: &str) -> NucleonId {
        NucleonId::new(id).unwrap()
    }

    #[test]
    fn emit_creates_ndjson_under_state_dir() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_TOKEN_INSTRUMENTATION_PATH");
        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260503-feb8"),
            Some(MoleculeKind::Task),
            "anthropic",
            1024,
            512,
            900,
            Some("anthropic-2026-04"),
        );
        let path = tmp.path().join(TOKEN_NDJSON_RELATIVE_PATH);
        assert!(path.exists(), "ndjson should be created at {path:?}");
        let events = read_token_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.tenant.as_str(), "operator");
        assert_eq!(ev.molecule_id.as_str(), "task-20260503-feb8");
        assert_eq!(ev.kind, Some(MoleculeKind::Task));
        assert_eq!(ev.backend, "anthropic");
        assert_eq!(ev.tokens_in, 1024);
        assert_eq!(ev.tokens_out, 512);
        assert_eq!(ev.cost_micros_estimated, 900);
        assert_eq!(
            ev.pricing_table_version.as_deref(),
            Some("anthropic-2026-04")
        );
    }

    #[test]
    fn emit_appends_multiple_events() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_TOKEN_INSTRUMENTATION_PATH");
        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260503-feb8"),
            Some(MoleculeKind::Task),
            "anthropic",
            10,
            20,
            0,
            None,
        );
        emit_token_usage(
            tmp.path(),
            &tenant("tenant_auditor"),
            &mol("task-20260503-cccc"),
            Some(MoleculeKind::Idea),
            "ollama",
            100,
            200,
            0,
            None,
        );
        let path = tmp.path().join(TOKEN_NDJSON_RELATIVE_PATH);
        let events = read_token_ndjson(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].tenant.as_str(), "tenant_auditor");
        assert_eq!(events[1].backend, "ollama");
    }

    #[test]
    fn pricing_table_version_omitted_when_none() {
        // Wire format must stay stable: omit-when-none discipline lets
        // future tools add the field without breaking external scrapers.
        let event = TokenUsage {
            tenant: tenant("operator"),
            molecule_id: mol("task-20260503-feb8"),
            kind: None,
            backend: "anthropic".into(),
            tokens_in: 0,
            tokens_out: 0,
            cost_micros_estimated: 0,
            pricing_table_version: None,
            timestamp: Utc::now(),
        };
        let s = serde_json::to_string(&event).unwrap();
        assert!(
            !s.contains("pricing_table_version"),
            "must be omitted when None: {s}"
        );
        assert!(
            !s.contains("\"kind\""),
            "kind must be omitted when None: {s}"
        );
    }

    #[test]
    fn read_returns_empty_when_missing() {
        let path = std::path::PathBuf::from("/tmp/cosmon-token-does-not-exist.jsonl");
        let events = read_token_ndjson(&path).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn env_var_overrides_state_dir_path() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let custom = tmp.path().join("custom").join("tokens.jsonl");
        std::env::set_var("COSMON_TOKEN_INSTRUMENTATION_PATH", &custom);
        let resolved = resolve_token_path(tmp.path());
        std::env::remove_var("COSMON_TOKEN_INSTRUMENTATION_PATH");
        assert_eq!(resolved, custom);
    }

    /// Build a `TokenUsage` event with explicit token counts — keeps the
    /// aggregation tests terse.
    fn usage(molecule: &str, tin: u64, tout: u64, cost: u64) -> TokenUsage {
        TokenUsage {
            tenant: tenant("operator"),
            molecule_id: mol(molecule),
            kind: Some(MoleculeKind::Task),
            backend: "anthropic".into(),
            tokens_in: tin,
            tokens_out: tout,
            cost_micros_estimated: cost,
            pricing_table_version: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn aggregate_by_molecule_sums_per_id() {
        let events = vec![
            usage("task-20260625-aaaa", 100, 50, 10),
            usage("task-20260625-aaaa", 200, 80, 20),
            usage("task-20260625-bbbb", 5, 3, 1),
        ];
        let by_mol = aggregate_by_molecule(&events);
        assert_eq!(by_mol.len(), 2);

        let a = by_mol.get(&mol("task-20260625-aaaa")).unwrap();
        assert_eq!(a.tokens_in, 300);
        assert_eq!(a.tokens_out, 130);
        assert_eq!(a.cost_micros_estimated, 30);
        assert_eq!(a.invocations, 2);
        assert_eq!(a.total_tokens(), 430);

        let b = by_mol.get(&mol("task-20260625-bbbb")).unwrap();
        assert_eq!(b.total_tokens(), 8);
        assert_eq!(b.invocations, 1);
    }

    #[test]
    fn molecule_token_totals_reads_canonical_sink() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_TOKEN_INSTRUMENTATION_PATH");

        // Two calls advancing the target molecule, one for an unrelated
        // molecule that must not leak into the sum.
        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260625-aaaa"),
            Some(MoleculeKind::Task),
            "anthropic",
            1000,
            400,
            900,
            None,
        );
        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260625-aaaa"),
            Some(MoleculeKind::Task),
            "anthropic",
            500,
            100,
            300,
            None,
        );
        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260625-bbbb"),
            Some(MoleculeKind::Task),
            "anthropic",
            7,
            7,
            7,
            None,
        );

        let totals = molecule_token_totals(tmp.path(), &mol("task-20260625-aaaa")).unwrap();
        assert_eq!(totals.tokens_in, 1500);
        assert_eq!(totals.tokens_out, 500);
        assert_eq!(totals.cost_micros_estimated, 1200);
        assert_eq!(totals.invocations, 2);
        assert_eq!(totals.total_tokens(), 2000);
    }

    #[test]
    fn molecule_token_totals_none_when_no_match() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_TOKEN_INSTRUMENTATION_PATH");

        // Absent sink → None (omit-if-none).
        assert!(molecule_token_totals(tmp.path(), &mol("task-20260625-zzzz")).is_none());

        emit_token_usage(
            tmp.path(),
            &tenant("operator"),
            &mol("task-20260625-aaaa"),
            Some(MoleculeKind::Task),
            "anthropic",
            10,
            10,
            0,
            None,
        );
        // Sink present but no matching molecule → still None.
        assert!(molecule_token_totals(tmp.path(), &mol("task-20260625-zzzz")).is_none());
    }
}
