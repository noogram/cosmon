// SPDX-License-Identifier: Apache-2.0

//! JSONL session log parser.
//!
//! Claude Code writes one JSON object per line to session logs. This module
//! does loose parsing via [`serde_json::Value`] so it survives schema evolution
//! across Claude Code versions — we extract only the fields we need and ignore
//! the rest.

use std::io::BufRead;
use std::path::Path;

use crate::energy::{SessionId, TokenCount};
use chrono::{DateTime, Utc};

use crate::error::ClaudionError;
use crate::types::{SessionLog, Turn};

/// Parse a Claude Code session JSONL file into a [`SessionLog`].
///
/// Reads every line, extracts assistant turns with their `usage` blocks,
/// and collects session-level metadata (`sessionId`, `slug`) from the
/// first line that carries them.
///
/// Lines that fail to parse as JSON are reported as errors with their
/// 1-based line number. Non-assistant lines are silently skipped for
/// turn extraction but still scanned for metadata.
///
/// # Errors
///
/// Returns [`ClaudionError::Io`] if the file cannot be opened or read,
/// or [`ClaudionError::JsonParse`] if a line is not valid JSON.
///
/// # Panics
///
/// Cannot panic in practice — the only `expect` is on constructing a
/// `SessionId` from the literal `"unknown"`, which is always valid.
pub fn parse_session(path: impl AsRef<Path>) -> Result<SessionLog, ClaudionError> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|e| ClaudionError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reader = std::io::BufReader::new(file);

    let mut turns = Vec::new();
    let mut session_id: Option<SessionId> = None;
    let mut project = String::new();
    let mut slug: Option<String> = None;
    let mut total_lines = 0usize;

    for (line_idx, line_result) in reader.lines().enumerate() {
        let line = line_result.map_err(|e| ClaudionError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        total_lines += 1;

        if line.trim().is_empty() {
            continue;
        }

        let value: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| ClaudionError::JsonParse {
                line: line_idx + 1,
                source: e,
            })?;

        // Extract session metadata from any line that has it.
        if session_id.is_none() {
            if let Some(sid) = value.get("sessionId").and_then(|v| v.as_str()) {
                session_id = SessionId::new(sid.to_string()).ok();
            }
        }
        if slug.is_none() {
            if let Some(s) = value.get("slug").and_then(|v| v.as_str()) {
                slug = Some(s.to_string());
            }
        }

        // Only assistant lines have usage blocks.
        let is_assistant = value.get("type").and_then(|v| v.as_str()) == Some("assistant");
        if !is_assistant {
            continue;
        }

        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };

        let timestamp = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);

        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from);

        let turn = Turn {
            index: turns.len(),
            timestamp,
            model,
            input_tokens: extract_token_count(usage, "input_tokens"),
            cache_creation_input_tokens: extract_token_count(usage, "cache_creation_input_tokens"),
            cache_read_input_tokens: extract_token_count(usage, "cache_read_input_tokens"),
            output_tokens: extract_token_count(usage, "output_tokens"),
        };
        turns.push(turn);
    }

    // Derive project from the filesystem path (grandparent directory name).
    if project.is_empty() {
        if let Some(parent) = path.parent() {
            project = parent
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
        }
    }

    let start_time = turns.first().map(|t| t.timestamp);
    let end_time = turns.last().map(|t| t.timestamp);

    // Fallback session_id from filename if not found in the log.
    let session_id = session_id.unwrap_or_else(|| {
        let stem = path.file_stem().map_or_else(
            || "unknown".to_string(),
            |s| s.to_string_lossy().to_string(),
        );
        // SAFETY: "unknown" is a valid non-empty string, so this cannot fail.
        #[allow(clippy::expect_used)]
        SessionId::new(stem)
            .unwrap_or_else(|_| SessionId::new("unknown".to_string()).expect("non-empty string"))
    });

    Ok(SessionLog {
        session_id,
        project,
        slug,
        start_time,
        end_time,
        turns,
        total_lines,
    })
}

/// Extract a u64 token count from a JSON usage object, defaulting to 0.
fn extract_token_count(usage: &serde_json::Value, field: &str) -> TokenCount {
    let n = usage
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    TokenCount::new(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_parse_single_assistant_turn() {
        let f = write_jsonl(&[
            r#"{"type":"user","sessionId":"abc-123","slug":"test-slug","message":{"role":"user","content":"hi"},"timestamp":"2026-04-06T10:00:00Z"}"#,
            r#"{"type":"assistant","sessionId":"abc-123","slug":"test-slug","timestamp":"2026-04-06T10:00:01Z","message":{"model":"claude-opus-4-6","role":"assistant","content":"hello","usage":{"input_tokens":100,"cache_creation_input_tokens":500,"cache_read_input_tokens":2000,"output_tokens":50}}}"#,
        ]);
        let log = parse_session(f.path()).unwrap();
        assert_eq!(log.session_id.as_str(), "abc-123");
        assert_eq!(log.slug.as_deref(), Some("test-slug"));
        assert_eq!(log.turns.len(), 1);

        let turn = &log.turns[0];
        assert_eq!(turn.index, 0);
        assert_eq!(turn.input_tokens, TokenCount::new(100));
        assert_eq!(turn.cache_creation_input_tokens, TokenCount::new(500));
        assert_eq!(turn.cache_read_input_tokens, TokenCount::new(2000));
        assert_eq!(turn.output_tokens, TokenCount::new(50));
        assert_eq!(turn.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn test_parse_skips_non_assistant_lines() {
        let f = write_jsonl(&[
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s1"}"#,
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            r#"{"type":"user","sessionId":"s1","message":{"role":"user","content":"hi"}}"#,
        ]);
        let log = parse_session(f.path()).unwrap();
        assert!(log.turns.is_empty());
        assert_eq!(log.total_lines, 3);
    }

    #[test]
    fn test_parse_multiple_turns() {
        let f = write_jsonl(&[
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-04-06T10:00:00Z","message":{"role":"assistant","usage":{"input_tokens":10,"output_tokens":5}}}"#,
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-04-06T10:01:00Z","message":{"role":"assistant","usage":{"input_tokens":20,"output_tokens":10}}}"#,
        ]);
        let log = parse_session(f.path()).unwrap();
        assert_eq!(log.turns.len(), 2);
        assert_eq!(log.turns[0].index, 0);
        assert_eq!(log.turns[1].index, 1);
    }

    #[test]
    fn test_parse_missing_usage_fields_default_to_zero() {
        let f = write_jsonl(&[
            r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-04-06T10:00:00Z","message":{"role":"assistant","usage":{"input_tokens":42}}}"#,
        ]);
        let log = parse_session(f.path()).unwrap();
        assert_eq!(log.turns[0].input_tokens, TokenCount::new(42));
        assert_eq!(log.turns[0].cache_creation_input_tokens, TokenCount::new(0));
        assert_eq!(log.turns[0].output_tokens, TokenCount::new(0));
    }

    #[test]
    fn test_parse_empty_file() {
        let f = write_jsonl(&[]);
        let log = parse_session(f.path()).unwrap();
        assert!(log.turns.is_empty());
        assert_eq!(log.total_lines, 0);
    }

    #[test]
    fn test_parse_session_id_from_filename() {
        // No sessionId in the data — falls back to filename.
        let f = write_jsonl(&[
            r#"{"type":"assistant","timestamp":"2026-04-06T10:00:00Z","message":{"role":"assistant","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ]);
        let log = parse_session(f.path()).unwrap();
        // Session ID should come from the tempfile name.
        assert!(!log.session_id.as_str().is_empty());
    }
}
