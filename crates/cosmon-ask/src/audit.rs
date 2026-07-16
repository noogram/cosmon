// SPDX-License-Identifier: AGPL-3.0-only

//! Audit log for `cs ask` invocations.
//!
//! Every invocation appends one line to
//! `.cosmon/state/ask.jsonl` so later analyses can measure hit-rate
//! (confidence ≥ floor without clarification), galaxy distribution,
//! and drift between the rule table and the intents operators type.
//!
//! The log format is deliberately tiny and flat — `jq` is the first
//! reader. Field names match the briefing: `ts, intent_text,
//! parsed_tokens, confidence, resolved_galaxy, formula, mol_id,
//! outcome`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::AskTokens;

/// Terminal outcome of an ask invocation. The variant directly
/// classifies the branch the CLI took, so counts per outcome over a
/// sliding window give the operator an honest picture of how often
/// the rule-path actually self-dispatches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Dispatched cleanly — no clarification, no override.
    Dispatched,
    /// Clarified then dispatched — operator accepted a prompt.
    ClarifiedThenDispatched,
    /// Operator chose to abort at an atomic question.
    Aborted,
    /// Running-quota gate refused the dispatch.
    QuotaRefused,
    /// Kill-switch `~/.cosmon/ask.off` was present.
    KillSwitched,
    /// Errored during resolution or dispatch — the CLI will surface
    /// the message separately; the log records only the tag.
    Errored,
}

/// One line of the audit log.
///
/// `mol_id` and `resolved_galaxy` are optional because not every
/// outcome resolves them (aborted at the question, quota-refused,
/// kill-switched).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditRecord {
    /// ISO-8601 UTC timestamp. The writer is a pure function; the
    /// caller produces the timestamp so unit tests can pin it.
    pub ts: String,
    /// Raw free text typed by the operator.
    pub intent_text: String,
    /// Tokens extracted by the parser.
    pub parsed_tokens: AskTokens,
    /// Confidence score the parser produced.
    pub confidence: f32,
    /// Galaxy name the pipeline resolved to, if any.
    pub resolved_galaxy: Option<String>,
    /// Formula id the dispatch used, if any.
    pub formula: Option<String>,
    /// Molecule id created by `cs nucleate`, if any.
    pub mol_id: Option<String>,
    /// Outcome tag.
    pub outcome: Outcome,
}

impl AuditRecord {
    /// Append one record to `path` as a single NDJSON line.
    ///
    /// Creates the file and any missing parents. Write failures are
    /// bubbled — the CLI chooses whether to swallow them (defensive
    /// audit logging is never allowed to block the hot path; see
    /// ADR on briefing seals) or surface them.
    ///
    /// # Errors
    ///
    /// Returns any `std::io::Error` raised by the filesystem.
    pub fn append(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::FormulaId;
    use cosmon_core::kind::MoleculeKind;

    fn record() -> AuditRecord {
        AuditRecord {
            ts: "2026-04-23T10:00:00Z".into(),
            intent_text: "fix the bug".into(),
            parsed_tokens: AskTokens {
                intent_verb: "fix".into(),
                kind: MoleculeKind::Issue,
                formula: FormulaId::new("task-work").unwrap(),
                galaxy_hint: Some("mailroom".into()),
                topic: "fix the bug".into(),
            },
            confidence: 0.95,
            resolved_galaxy: Some("mailroom".into()),
            formula: Some("task-work".into()),
            mol_id: Some("task-20260423-9999".into()),
            outcome: Outcome::Dispatched,
        }
    }

    #[test]
    fn append_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".cosmon/state/ask.jsonl");
        let r = record();
        r.append(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let round: AuditRecord = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(round, r);
    }

    #[test]
    fn append_is_append_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ask.jsonl");
        let r = record();
        r.append(&path).unwrap();
        r.append(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }
}
