// SPDX-License-Identifier: Apache-2.0

//! The Claude Code adapter.
//!
//! Claude writes `~/.claude/projects/<sanitised-cwd>/<session-uuid>.jsonl`, one
//! JSON object per line, and repeats the session envelope — `sessionId`, `cwd`,
//! `gitBranch`, `version` — on *every* record (Trace A of ADR-168).
//!
//! Enumeration reuses [`claudion::discover_sessions`], which is the existing
//! measured probe and stays the owner of the "where are the logs" question.
//! Everything after that is this module's, and one difference from the existing
//! code is deliberate and load-bearing:
//!
//! **The project directory name is never decoded.** `sanitize_path` maps every
//! non-alphanumeric byte to `-`, so `…/cosmon/.worktrees/task-X` and
//! `…/cosmon--worktrees/task-X` produce the same directory name and no
//! inverse exists (probe P6). The `cwd` is read from the `cwd` field carried
//! inside the log, and repo identity is resolved from that.

use std::path::{Path, PathBuf};

use crate::cursor::RawLine;
use crate::error::ProbeError;
use crate::event::{content_chars, timestamp_at, SessionEvent, SessionEventKind, TurnUsage};
use crate::port::{mtime_of, DiscoveryFilter, ProviderSessionRef, SessionProbe};
use crate::repo::RepoIdentity;
use crate::selector::{NativeSessionId, ProviderName};
use claudion::TokenCount;

/// How many complete lines of a log the envelope scan reads before giving up.
///
/// Claude repeats the envelope on every record, so the first line almost always
/// answers; the budget exists so a pathological log (a huge
/// `file-history-snapshot` first record, a run of `attachment` lines) cannot
/// turn discovery into a full parse of every session on the host.
const ENVELOPE_SCAN_LINES: usize = 64;

/// The Claude Code session probe.
pub struct ClaudeProbe {
    provider: ProviderName,
    projects_root: PathBuf,
}

impl ClaudeProbe {
    /// A probe reading a specific `projects/` root.
    ///
    /// Injected rather than discovered because a worker's Claude configuration
    /// directory is not always the ambient one — `cb next`-derived
    /// `~/.claude-accounts/<email>/` appears in no inherited variable, a fact
    /// the realized-model watcher already had to learn.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] never, in practice: the provider name
    /// is the literal `claude`. The signature is fallible so the constructor
    /// composes with a registry built from configuration.
    pub fn new(projects_root: impl Into<PathBuf>) -> Result<Self, ProbeError> {
        Ok(Self {
            provider: ProviderName::new("claude")?,
            projects_root: projects_root.into(),
        })
    }

    /// A probe reading the ambient `$HOME/.claude/projects`.
    ///
    /// # Errors
    ///
    /// [`ProbeError::InvalidIdentifier`] if `HOME` is unset — there is no
    /// default path to fall back to, and inventing one would be an
    /// authority-by-default of exactly the kind FAIL-CLOSED-AUTHORITY refuses.
    pub fn from_home() -> Result<Self, ProbeError> {
        let home = std::env::var("HOME").map_err(|_| {
            ProbeError::InvalidIdentifier("HOME is unset — no Claude projects root".to_string())
        })?;
        Self::new(PathBuf::from(home).join(".claude").join("projects"))
    }

    /// The `projects/` root this probe reads.
    #[must_use]
    pub fn projects_root(&self) -> &Path {
        &self.projects_root
    }
}

/// The envelope facts a Claude log carries about itself.
struct Envelope {
    session_id: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    title: Option<String>,
    first_timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Scan the head of a Claude log for its session envelope.
///
/// Read-only and bounded: at most [`ENVELOPE_SCAN_LINES`] complete lines.
fn scan_envelope(path: &Path) -> Envelope {
    use std::io::BufRead as _;

    let mut env = Envelope {
        session_id: None,
        cwd: None,
        git_branch: None,
        title: None,
        first_timestamp: None,
    };
    let Ok(file) = std::fs::File::open(path) else {
        return env;
    };
    for line in std::io::BufReader::new(file)
        .lines()
        .take(ENVELOPE_SCAN_LINES)
    {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let take = |field: &str| {
            value
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        env.session_id = env.session_id.or_else(|| take("sessionId"));
        env.cwd = env.cwd.or_else(|| take("cwd"));
        env.git_branch = env.git_branch.or_else(|| take("gitBranch"));
        env.title = env.title.or_else(|| take("aiTitle"));
        env.first_timestamp = env
            .first_timestamp
            .or_else(|| timestamp_at(&value, "timestamp"));
    }
    env
}

impl SessionProbe for ClaudeProbe {
    fn provider(&self) -> &ProviderName {
        &self.provider
    }

    fn discover(&self, filter: &DiscoveryFilter) -> Result<Vec<ProviderSessionRef>, ProbeError> {
        // A missing root is "no sessions", not a fault: a host with no Claude
        // installed is a normal host. `claudion` reports both cases the same
        // way, and both are legitimately empty here.
        let Ok(paths) = claudion::discover_sessions(&self.projects_root) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for sp in paths {
            let env = scan_envelope(&sp.path);
            // The id in the records wins over the filename; they agree in
            // every observed log, and when they do not, the record is what the
            // running session believes about itself.
            let native = env
                .session_id
                .clone()
                .unwrap_or_else(|| sp.session_id.as_str().to_string());
            let Ok(native_session_id) = NativeSessionId::new(native) else {
                continue; // Unaddressable session: skip rather than invent a key.
            };
            let cwd = env.cwd.clone().map(PathBuf::from);
            let session = ProviderSessionRef {
                provider: self.provider.clone(),
                native_session_id,
                repo_identity: cwd.as_ref().and_then(RepoIdentity::resolve),
                cwd,
                source_locator: sp.path.clone(),
                display_name: env.title.clone(),
                started_at: env.first_timestamp,
                last_observed_at: mtime_of(&sp.path),
            };
            if filter.accepts(&session) {
                out.push(session);
            }
        }
        out.sort_by(|a, b| a.source_locator.cmp(&b.source_locator));
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
        let message = value.get("message");
        let content = message.and_then(|m| m.get("content"));

        // Claude marks no record as "the session started" — it repeats the
        // envelope on every line instead. The first record of a generation is
        // therefore the announcement, and the rule is stateless: offset 0.
        if line.offset == 0 {
            return SessionEvent {
                offset: line.offset,
                at,
                kind: SessionEventKind::SessionStarted {
                    cwd: value
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    git_branch: value
                        .get("gitBranch")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    model_provider: None,
                },
            };
        }

        let kind = match record.as_str() {
            "user" => SessionEventKind::UserMessage {
                chars: content_chars(content),
            },
            "assistant" => SessionEventKind::AssistantMessage {
                model: message
                    .and_then(|m| m.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                chars: content_chars(content),
                usage: message.and_then(|m| m.get("usage")).map(usage_from_claude),
            },
            "" => SessionEventKind::Other {
                record: "<untyped>".to_string(),
            },
            other => SessionEventKind::Other {
                record: other.to_string(),
            },
        };

        SessionEvent {
            offset: line.offset,
            at,
            kind,
        }
    }
}

/// Map Claude's four disjoint counters onto the normalised three.
fn usage_from_claude(usage: &serde_json::Value) -> TurnUsage {
    let field = |name: &str| {
        usage
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let fresh = field("input_tokens");
    let creation = field("cache_creation_input_tokens");
    let read = field("cache_read_input_tokens");
    TurnUsage {
        input_total: TokenCount::new(fresh + creation + read),
        cached_input: TokenCount::new(read),
        output_total: TokenCount::new(field("output_tokens")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> ClaudeProbe {
        ClaudeProbe::new("/nonexistent").unwrap()
    }

    /// A line from the middle of a log — offset 0 is the session
    /// announcement, which these cases are not about.
    fn line(text: &str) -> RawLine {
        RawLine {
            offset: 1,
            text: text.to_string(),
        }
    }

    #[test]
    fn the_first_record_announces_the_session_from_inside_the_log() {
        let ev = probe().normalize(&RawLine {
            offset: 0,
            text: r#"{"type":"user","sessionId":"s1","cwd":"/fixture/galaxy","gitBranch":"main"}"#
                .to_string(),
        });
        assert_eq!(
            ev.kind,
            SessionEventKind::SessionStarted {
                cwd: Some("/fixture/galaxy".to_string()),
                git_branch: Some("main".to_string()),
                model_provider: None,
            }
        );
    }

    #[test]
    fn an_assistant_record_normalises_to_a_turn_with_summed_input() {
        let ev = probe().normalize(&line(
            r#"{"type":"assistant","timestamp":"2026-08-01T00:06:35.485Z","message":{"model":"claude-opus-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":7}}}"#,
        ));
        match ev.kind {
            SessionEventKind::AssistantMessage {
                model,
                chars,
                usage,
            } => {
                assert_eq!(model.as_deref(), Some("claude-opus-5"));
                assert_eq!(chars, 2);
                let usage = usage.unwrap();
                assert_eq!(usage.input_total, TokenCount::new(1110));
                assert_eq!(usage.cached_input, TokenCount::new(1000));
                assert_eq!(usage.output_total, TokenCount::new(7));
            }
            other => panic!("expected an assistant turn, got {other:?}"),
        }
        assert!(ev.at.is_some());
    }

    #[test]
    fn an_unmapped_record_keeps_its_provider_name_and_nothing_else() {
        let ev = probe().normalize(&line(r#"{"type":"file-history-snapshot","snapshot":{}}"#));
        assert_eq!(
            ev.kind,
            SessionEventKind::Other {
                record: "file-history-snapshot".to_string()
            }
        );
    }

    #[test]
    fn a_garbage_line_is_an_event_not_an_error() {
        let ev = probe().normalize(&line("{not json"));
        assert_eq!(ev.kind, SessionEventKind::Unparseable);
    }

    #[test]
    fn a_missing_projects_root_is_zero_sessions_not_a_fault() {
        let found = probe().discover(&DiscoveryFilter::all()).unwrap();
        assert!(found.is_empty());
    }
}
