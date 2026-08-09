// SPDX-License-Identifier: Apache-2.0

//! The Codex adapter.
//!
//! Codex writes `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`.
//! Unlike Claude it announces itself **once**, in a leading `session_meta`
//! record carrying `session_id`, `cwd` and git branch; the rest of the file is
//! `response_item` / `event_msg` / `turn_context` traffic (Trace B of ADR-168).
//!
//! Two things this adapter does that the existing Codex reader does not:
//!
//! - **It returns every session in a working directory.**
//!   `resolve_codex_session_by_cwd` returns the most-recently-modified match,
//!   which silently collapses two sessions in one worktree into one (probe P6)
//!   — the mission's own falsifier 3.
//! - **It normalises the quota telemetry.** Codex publishes `rate_limits` on
//!   every `token_count` event. ADR-168's D3.1 refuses to *act* on that signal;
//!   it does not refuse to *see* it, and a co-pilot that can read its own fuel
//!   gauge can tell the operator what it knows.

use std::path::{Path, PathBuf};

use crate::cursor::RawLine;
use crate::error::ProbeError;
use crate::event::{
    content_chars, timestamp_at, QuotaReading, SessionEvent, SessionEventKind, TurnUsage,
};
use crate::port::{mtime_of, DiscoveryFilter, ProviderSessionRef, SessionProbe};
use crate::repo::RepoIdentity;
use crate::selector::{NativeSessionId, ProviderName};
use claudion::TokenCount;

/// How many complete lines the `session_meta` scan reads before giving up.
/// The record is the first line of every observed rollout; the budget is
/// slack, not an expectation.
const META_SCAN_LINES: usize = 8;

/// The Codex session probe.
pub struct CodexProbe {
    provider: ProviderName,
    sessions_root: PathBuf,
}

impl CodexProbe {
    /// A probe reading a specific `sessions/` root.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] never in practice — the provider name
    /// is the literal `codex`; the signature composes with configuration-built
    /// registries.
    pub fn new(sessions_root: impl Into<PathBuf>) -> Result<Self, ProbeError> {
        Ok(Self {
            provider: ProviderName::new("codex")?,
            sessions_root: sessions_root.into(),
        })
    }

    /// A probe reading the ambient `$HOME/.codex/sessions`.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] if `HOME` is unset.
    pub fn from_home() -> Result<Self, ProbeError> {
        let home = std::env::var("HOME").map_err(|_| {
            ProbeError::InvalidIdentifier("HOME is unset — no Codex sessions root".to_string())
        })?;
        Self::new(PathBuf::from(home).join(".codex").join("sessions"))
    }

    /// The `sessions/` root this probe reads.
    #[must_use]
    pub fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }
}

/// Every `*.jsonl` under a date-bucketed sessions tree, sorted for determinism.
fn rollout_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// The `session_meta` payload of a rollout, if it has one in its first lines.
fn scan_meta(path: &Path) -> Option<serde_json::Value> {
    use std::io::BufRead as _;

    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().take(META_SCAN_LINES) {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
            return Some(value);
        }
    }
    None
}

/// `payload.<field>`, with a top-level fallback — the shape both observed
/// rollout generations use.
fn meta_str<'a>(meta: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    meta.get("payload")
        .and_then(|p| p.get(field))
        .or_else(|| meta.get(field))
        .and_then(serde_json::Value::as_str)
}

impl SessionProbe for CodexProbe {
    fn provider(&self) -> &ProviderName {
        &self.provider
    }

    fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<ProviderSessionRef>, ProbeError> {
        let mut out = Vec::new();
        for path in rollout_files(&self.sessions_root) {
            let Some(meta) = scan_meta(&path) else {
                continue; // Not a rollout, or one that never announced itself.
            };
            let Some(native) = meta_str(&meta, "session_id") else {
                continue; // No native id ⇒ not addressable ⇒ not offered.
            };
            let Ok(native_session_id) = NativeSessionId::new(native.to_string()) else {
                continue;
            };
            let cwd = meta_str(&meta, "cwd").map(PathBuf::from);
            let session = ProviderSessionRef {
                provider: self.provider.clone(),
                native_session_id,
                repo_identity: cwd.as_ref().and_then(RepoIdentity::resolve),
                cwd,
                source_locator: path.clone(),
                // Codex records no title. An unnamed session is the normal
                // case here, which is precisely why nothing may key on a name.
                display_name: None,
                started_at: timestamp_at(&meta, "timestamp"),
                last_observed_at: mtime_of(&path),
            };
            if filter.accepts(&session) {
                out.push(session);
            }
        }
        Ok(out)
    }

    fn normalize(&self, line: &RawLine) -> SessionEvent {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line.text) else {
            return SessionEvent {
                offset: line.offset,
                at: None,
                kind: SessionEventKind::Unparseable,
            };
        };
        let at = timestamp_at(&value, "timestamp");
        let record = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let payload = value.get("payload");
        let payload_type = payload
            .and_then(|p| p.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        let kind = match (record.as_str(), payload_type) {
            ("session_meta", _) => SessionEventKind::SessionStarted {
                cwd: meta_str(&value, "cwd").map(str::to_string),
                git_branch: payload
                    .and_then(|p| p.get("git"))
                    .and_then(|g| g.get("branch"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                model_provider: meta_str(&value, "model_provider").map(str::to_string),
            },
            (_, "user_message") => SessionEventKind::UserMessage {
                chars: payload
                    .and_then(|p| p.get("message"))
                    .map_or(0, |m| content_chars(Some(m))),
            },
            (_, "agent_message") => SessionEventKind::AssistantMessage {
                model: None,
                chars: payload
                    .and_then(|p| p.get("message"))
                    .map_or(0, |m| content_chars(Some(m))),
                usage: None,
                // Codex publishes no per-turn transport-failure flag of its
                // own. `false` is the honest answer — "this provider does not
                // say" — and it fails toward *not* propelling, which is the
                // safe direction for a signal whose only consumer nudges a
                // live worker.
                api_error: false,
            },
            (_, "token_count") => match payload.and_then(usage_from_codex) {
                Some(usage) => SessionEventKind::TokenUsage {
                    usage,
                    // `total_token_usage` is maintained cumulatively by codex
                    // itself; summing successive readings double-counts.
                    cumulative: true,
                },
                None => quota_or_other(payload, &record),
            },
            (_, "context_compacted") | ("compacted", _) => SessionEventKind::ContextCompacted,
            (r, "") => SessionEventKind::Other {
                record: r.to_string(),
            },
            (r, p) => SessionEventKind::Other {
                record: format!("{r}/{p}"),
            },
        };

        // A `token_count` carries both meters. The usage half becomes the
        // event above; the quota half is emitted only when there is no usage
        // to report, so one line still maps to exactly one event.
        SessionEvent {
            offset: line.offset,
            at,
            kind,
        }
    }
}

/// The quota reading of a `token_count` payload, or a residual `Other`.
fn quota_or_other(payload: Option<&serde_json::Value>, record: &str) -> SessionEventKind {
    match payload.and_then(quota_from_codex) {
        Some(quota) => SessionEventKind::Quota(quota),
        None => SessionEventKind::Other {
            record: format!("{record}/token_count"),
        },
    }
}

/// Codex's cumulative counters, mapped onto the normalised three.
fn usage_from_codex(payload: &serde_json::Value) -> Option<TurnUsage> {
    let totals = payload
        .get("info")
        .and_then(|i| i.get("total_token_usage"))
        .or_else(|| payload.get("total_token_usage"))?;
    let field = |name: &str| {
        totals
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Some(TurnUsage {
        // Codex's `input_tokens` already includes the cached portion.
        input_total: TokenCount::new(field("input_tokens")),
        cached_input: TokenCount::new(field("cached_input_tokens")),
        output_total: TokenCount::new(field("output_tokens")),
    })
}

/// Codex's `rate_limits` block, mapped onto [`QuotaReading`].
fn quota_from_codex(payload: &serde_json::Value) -> Option<QuotaReading> {
    let limits = payload.get("rate_limits")?;
    let primary = limits.get("primary");
    Some(QuotaReading {
        used_percent: primary
            .and_then(|p| p.get("used_percent"))
            .and_then(serde_json::Value::as_f64),
        window_minutes: primary
            .and_then(|p| p.get("window_minutes"))
            .and_then(serde_json::Value::as_u64),
        resets_at_epoch: primary
            .and_then(|p| p.get("resets_at"))
            .and_then(serde_json::Value::as_i64),
        limit_reached: limits
            .get("rate_limit_reached_type")
            .is_some_and(|v| !v.is_null()),
    })
}

/// The quota reading carried by a `token_count` line, when it has one.
///
/// Exposed beside [`SessionProbe::normalize`] rather than folded into it
/// because one line maps to one event, and a `token_count` line carries two
/// meters. The usage meter wins the event; a caller that watches quota reads
/// it here from the same line.
#[must_use]
pub fn quota_in_line(line: &RawLine) -> Option<QuotaReading> {
    let value = serde_json::from_str::<serde_json::Value>(&line.text).ok()?;
    quota_from_codex(value.get("payload")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> CodexProbe {
        CodexProbe::new("/nonexistent").unwrap()
    }

    fn line(text: &str) -> RawLine {
        RawLine {
            offset: 0,
            text: text.to_string(),
        }
    }

    #[test]
    fn session_meta_announces_cwd_branch_and_provider() {
        let ev = probe().normalize(&line(
            r#"{"type":"session_meta","payload":{"session_id":"c-1","cwd":"/fixture/galaxy","model_provider":"openai","git":{"branch":"main"}}}"#,
        ));
        assert_eq!(
            ev.kind,
            SessionEventKind::SessionStarted {
                cwd: Some("/fixture/galaxy".to_string()),
                git_branch: Some("main".to_string()),
                model_provider: Some("openai".to_string()),
            }
        );
    }

    #[test]
    fn token_count_is_reported_as_cumulative() {
        let ev = probe().normalize(&line(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900,"cached_input_tokens":800,"output_tokens":40}},"rate_limits":{"primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1785000000},"rate_limit_reached_type":null}}}"#,
        ));
        match ev.kind {
            SessionEventKind::TokenUsage { usage, cumulative } => {
                assert!(cumulative, "summing these would double-count");
                assert_eq!(usage.input_total, TokenCount::new(900));
                assert_eq!(usage.cached_input, TokenCount::new(800));
            }
            other => panic!("expected cumulative usage, got {other:?}"),
        }
    }

    #[test]
    fn the_quota_meter_of_the_same_line_is_readable_beside_the_event() {
        let raw = line(
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1785000000},"rate_limit_reached_type":null}}}"#,
        );
        let quota = quota_in_line(&raw).unwrap();
        assert_eq!(quota.used_percent, Some(12.5));
        assert_eq!(quota.window_minutes, Some(10080));
        assert!(!quota.limit_reached);

        // No usage half on this line, so the event carries the quota instead.
        assert!(matches!(
            probe().normalize(&raw).kind,
            SessionEventKind::Quota(_)
        ));
    }

    #[test]
    fn an_unmapped_payload_keeps_both_halves_of_its_provider_name() {
        let ev = probe().normalize(&line(
            r#"{"type":"response_item","payload":{"type":"reasoning"}}"#,
        ));
        assert_eq!(
            ev.kind,
            SessionEventKind::Other {
                record: "response_item/reasoning".to_string()
            }
        );
    }

    #[test]
    fn a_missing_sessions_root_is_zero_sessions_not_a_fault() {
        assert!(probe()
            .discover(&DiscoveryFilter::all())
            .unwrap()
            .is_empty());
    }
}
