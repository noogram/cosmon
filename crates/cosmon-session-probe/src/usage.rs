// SPDX-License-Identifier: Apache-2.0

//! Retrospective token snapshots over every provider session for one cwd.
//!
//! Unlike the live session port, this reader is deliberately whole-file and
//! harvest-shaped: it runs once before teardown, sums every session attributed
//! to the recorded cwd, and returns no value when the provider wrote no usage.

use std::collections::HashSet;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ProbeError;

/// Provider transcript that supplied a token snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TranscriptTokenSource {
    /// Claude Code project transcripts.
    #[default]
    ClaudeTranscript,
    /// Codex rollout logs.
    CodexRollout,
}

impl TranscriptTokenSource {
    /// Stable wire spelling used by the token meter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeTranscript => "claude_transcript",
            Self::CodexRollout => "codex_rollout",
        }
    }
}

/// Lower-bound token usage observed in provider transcripts for one cwd.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptUsageSnapshot {
    /// Provider transcript that supplied the counters.
    pub source: TranscriptTokenSource,
    /// Fresh input tokens, excluding both cache reads and cache creation.
    pub tokens_in: u64,
    /// Input tokens read from a provider cache.
    pub cache_read_tokens: u64,
    /// Input tokens written into a provider cache, when that provider reports it.
    pub cache_creation_tokens: Option<u64>,
    /// Output tokens recorded by the provider.
    pub tokens_out: u64,
    /// Distinct assistant turns represented by the snapshot.
    pub invocations: u64,
}

impl TranscriptUsageSnapshot {
    fn is_empty(self) -> bool {
        self.invocations == 0
    }
}

/// Read the transcript snapshot for `cwd` from the provider selected at dispatch.
///
/// `claude_project` is the already-resolved project directory beneath
/// `$HOME/.claude/projects`; `codex_sessions` is `$HOME/.codex/sessions`. The
/// cwd carried *inside* every accepted transcript must canonicalise to `cwd`;
/// directory names are never trusted for attribution. Unknown adapters return
/// no snapshot rather than guessing at another provider.
///
/// # Errors
///
/// Returns [`ProbeError::Io`] when an encountered transcript cannot be read.
pub fn snapshot_usage_for_cwd(
    claude_project: &Path,
    codex_sessions: &Path,
    cwd: &Path,
    adapter: Option<&str>,
) -> Result<Option<TranscriptUsageSnapshot>, ProbeError> {
    match adapter {
        Some("claude") => claude_snapshot(claude_project, cwd),
        Some("codex") => codex_snapshot(codex_sessions, cwd),
        _ => Ok(None),
    }
}

fn canonical_eq(recorded: &str, expected: &Path) -> bool {
    let recorded = PathBuf::from(recorded);
    let recorded = std::fs::canonicalize(&recorded).unwrap_or(recorded);
    let expected = std::fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    recorded == expected
}

fn jsonl_files(root: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    files
}

fn lines(path: &Path) -> Result<impl Iterator<Item = Result<String, std::io::Error>>, ProbeError> {
    let file = std::fs::File::open(path).map_err(|source| ProbeError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(std::io::BufReader::new(file).lines())
}

fn claude_snapshot(
    project: &Path,
    cwd: &Path,
) -> Result<Option<TranscriptUsageSnapshot>, ProbeError> {
    let mut snapshot = TranscriptUsageSnapshot {
        source: TranscriptTokenSource::ClaudeTranscript,
        cache_creation_tokens: Some(0),
        ..TranscriptUsageSnapshot::default()
    };
    let mut seen_messages = HashSet::new();

    for path in jsonl_files(project) {
        for line in lines(&path)? {
            let line = line.map_err(|source| ProbeError::Io {
                path: path.clone(),
                source,
            })?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("type").and_then(serde_json::Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(recorded_cwd) = value.get("cwd").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !canonical_eq(recorded_cwd, cwd) {
                continue;
            }
            let Some(message) = value.get("message") else {
                continue;
            };
            let Some(usage) = message.get("usage") else {
                continue;
            };
            let identity = message
                .get("id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("uuid").and_then(serde_json::Value::as_str));
            if identity.is_some_and(|id| !seen_messages.insert(id.to_owned())) {
                continue;
            }
            let field = |name: &str| {
                usage
                    .get(name)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            snapshot.tokens_in = snapshot.tokens_in.saturating_add(field("input_tokens"));
            snapshot.cache_read_tokens = snapshot
                .cache_read_tokens
                .saturating_add(field("cache_read_input_tokens"));
            snapshot.cache_creation_tokens = snapshot
                .cache_creation_tokens
                .map(|total| total.saturating_add(field("cache_creation_input_tokens")));
            snapshot.tokens_out = snapshot.tokens_out.saturating_add(field("output_tokens"));
            snapshot.invocations = snapshot.invocations.saturating_add(1);
        }
    }

    Ok((!snapshot.is_empty()).then_some(snapshot))
}

fn codex_snapshot(
    sessions_root: &Path,
    cwd: &Path,
) -> Result<Option<TranscriptUsageSnapshot>, ProbeError> {
    let mut snapshot = TranscriptUsageSnapshot {
        source: TranscriptTokenSource::CodexRollout,
        cache_creation_tokens: None,
        ..TranscriptUsageSnapshot::default()
    };

    for path in jsonl_files(sessions_root) {
        let mut matches_cwd = false;
        let mut last: Option<(u64, u64, u64)> = None;
        let mut distinct_totals = HashSet::new();
        for line in lines(&path)? {
            let line = line.map_err(|source| ProbeError::Io {
                path: path.clone(),
                source,
            })?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
                matches_cwd = value
                    .get("payload")
                    .and_then(|payload| payload.get("cwd"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|recorded| canonical_eq(recorded, cwd));
                continue;
            }
            if !matches_cwd {
                continue;
            }
            let Some(total) = value
                .get("payload")
                .and_then(|payload| payload.get("info"))
                .and_then(|info| info.get("total_token_usage"))
            else {
                continue;
            };
            let field = |name: &str| {
                total
                    .get(name)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            let counters = (
                field("input_tokens"),
                field("cached_input_tokens"),
                field("output_tokens"),
            );
            last = Some(counters);
            distinct_totals.insert(counters);
        }
        if !matches_cwd {
            continue;
        }
        if let Some((input_total, cached, output)) = last {
            snapshot.tokens_in = snapshot
                .tokens_in
                .saturating_add(input_total.saturating_sub(cached));
            snapshot.cache_read_tokens = snapshot.cache_read_tokens.saturating_add(cached);
            snapshot.tokens_out = snapshot.tokens_out.saturating_add(output);
            snapshot.invocations = snapshot
                .invocations
                .saturating_add(distinct_totals.len() as u64);
        }
    }

    Ok((!snapshot.is_empty()).then_some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_sums_all_sessions_subagents_and_deduplicates_message_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("tenant/.worktrees/task-1");
        std::fs::create_dir_all(&cwd).unwrap();
        let projects = tmp.path().join("home/.claude/projects");
        let project = projects.join("project");
        let subagents = project.join("session/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let row = |id: &str, fresh: u64, creation: u64, read: u64, output: u64| {
            serde_json::json!({
                "type": "assistant", "cwd": cwd, "message": {
                    "id": id, "usage": {"input_tokens": fresh,
                    "cache_creation_input_tokens": creation,
                    "cache_read_input_tokens": read, "output_tokens": output}
                }
            })
            .to_string()
        };
        let repeated = row("msg-1", 10, 100, 1000, 7);
        std::fs::write(
            project.join("one.jsonl"),
            format!("{repeated}\n{repeated}\n"),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent.jsonl"),
            format!("{}\n", row("msg-2", 5, 2, 3, 4)),
        )
        .unwrap();

        let got = snapshot_usage_for_cwd(&project, Path::new("unused"), &cwd, Some("claude"))
            .unwrap()
            .unwrap();
        assert_eq!(got.tokens_in, 15);
        assert_eq!(got.cache_creation_tokens, Some(102));
        assert_eq!(got.cache_read_tokens, 1003);
        assert_eq!(got.tokens_out, 11);
        assert_eq!(got.invocations, 2);
    }

    #[test]
    fn codex_uses_the_last_cumulative_total_per_rollout() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("tenant/.worktrees/task-1");
        std::fs::create_dir_all(&cwd).unwrap();
        let sessions = tmp.path().join("home/.codex/sessions/2026/09/25");
        std::fs::create_dir_all(&sessions).unwrap();
        let meta = serde_json::json!({"type":"session_meta","payload":{"cwd":cwd}});
        let count = |input: u64, cached: u64, output: u64| {
            serde_json::json!({
                "type":"event_msg","payload":{"info":{"total_token_usage":{
                    "input_tokens":input,"cached_input_tokens":cached,"output_tokens":output
                }}}
            })
        };
        std::fs::write(
            sessions.join("rollout.jsonl"),
            format!(
                "{meta}\n{}\n{}\n",
                count(900, 800, 40),
                count(1800, 1600, 90)
            ),
        )
        .unwrap();

        let got = snapshot_usage_for_cwd(
            Path::new("unused"),
            &tmp.path().join("home/.codex/sessions"),
            &cwd,
            Some("codex"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(got.tokens_in, 200);
        assert_eq!(got.cache_read_tokens, 1600);
        assert_eq!(got.cache_creation_tokens, None);
        assert_eq!(got.tokens_out, 90);
        assert_eq!(got.invocations, 2);
    }
}
