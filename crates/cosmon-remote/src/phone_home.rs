// SPDX-License-Identifier: AGPL-3.0-only

//! Passive opt-out remontée — the client's gesture is to do nothing.
//!
//! When a command fails in a way that predicts abandonment (503,
//! 502 on the PKCE/token-exchange class, a burst of write 4xx), the
//! CLI prints **one line** saying that the `request_id` + error code
//! can be sent to the operator — nothing else, no casier data — and
//! queues exactly that pair. The pending pair rides the next
//! successful request as the `X-Cosmon-Phone-Home` header; the
//! adapter materialises it next to the audit envelopes where the
//! patrouille-abandon reads it.
//!
//! The engaged client disables it with one gesture
//! (`config set phone-home off`); the abandoning client does nothing,
//! and that is precisely when the signal is most needed — the inverse
//! of the exit form.
//! D-AVATAR-1 holds: the client cuts, the instance never imposes.
//!
//! # Anti-leak discipline
//!
//! The queue and the header carry **only** `request_id` + stable error
//! code, both sanitised to `[A-Za-z0-9._-]` and length-capped. Never
//! an artifact name or body, never the `sub`, never a URL.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Wire header carrying pending reports on the next request.
pub const HEADER: &str = "x-cosmon-phone-home";

/// Env override for the phone-home spool directory (tests; also lets
/// an operator relocate it). Default: `<config_dir>/cosmon-remote/phone-home/`.
pub const ENV_DIR: &str = "COSMON_REMOTE_PHONE_HOME_DIR";

/// Maximum reports a single header carries — bounded so the header
/// stays a whisper, not a payload.
pub const MAX_REPORTS_PER_HEADER: usize = 8;

/// Maximum length of each sanitised token (request_id or code).
const MAX_TOKEN_LEN: usize = 64;

/// Burst threshold: the Nth write-4xx inside [`BURST_WINDOW_SECS`]
/// becomes an abandonment predictor (Jordan took three opaque 4xx
/// before giving up).
const BURST_MIN: usize = 3;
const BURST_WINDOW_SECS: i64 = 600;

/// Resolve the spool directory. `None` when no config dir resolves
/// (phone-home then silently disables itself — never an error path).
#[must_use]
pub fn dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var(ENV_DIR) {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    dirs::config_dir().map(|b| b.join("cosmon-remote").join("phone-home"))
}

/// One queued report — only the pair the one-liner promised, plus the
/// host so a report never rides a request to a different instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReport {
    /// Target instance base URL the failure came from.
    pub host: String,
    /// `request_id` echoed by the failing response (may be empty when
    /// the failure carried none — e.g. connection-level 502).
    pub request_id: String,
    /// Stable error label, prefixed with the HTTP status (`"503 \
    /// tackle_unavailable"` → sanitised `503_tackle_unavailable`).
    pub code: String,
    /// RFC 3339 UTC timestamp of the failure.
    pub at: String,
}

/// Keep only header-safe charset (`[A-Za-z0-9._-]`, spaces become
/// `_`) and cap the length. Returns `None` when nothing meaningful
/// survives — a token without a single alphanumeric is dropped rather
/// than sent as garbage. Mirrors the adapter-side gate.
#[must_use]
pub fn sanitize_token(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                Some(c)
            } else if c == ' ' {
                Some('_')
            } else {
                None
            }
        })
        .take(MAX_TOKEN_LEN)
        .collect();
    if cleaned.chars().any(|c| c.is_ascii_alphanumeric()) {
        Some(cleaned)
    } else {
        None
    }
}

fn pending_path(dir: &Path) -> PathBuf {
    dir.join("pending.jsonl")
}

fn journal_path(dir: &Path) -> PathBuf {
    dir.join("failures.jsonl")
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    host: String,
    status: u16,
    at: chrono::DateTime<chrono::Utc>,
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn write_jsonl<T: Serialize>(path: &Path, items: &[T]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = String::new();
    for item in items {
        if let Ok(line) = serde_json::to_string(item) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let _ = std::fs::write(path, out);
}

/// Pure predictor: does this failure predict abandonment?
///
/// - any 503 (the universal first-tackle wall — Claude not logged);
/// - any 502 (the PKCE/token-exchange failure class);
/// - the [`BURST_MIN`]-th 4xx within [`BURST_WINDOW_SECS`] (the
///   Jordan rafale — `recent_4xx` includes the current failure).
#[must_use]
pub fn is_abandon_predictor(status: u16, recent_4xx: usize) -> bool {
    match status {
        503 | 502 => true,
        400..=499 => recent_4xx >= BURST_MIN,
        _ => false,
    }
}

/// The single line shown when a predictor fires. One sentence, the
/// promise of what is sent (and what never is), and the off switch.
#[must_use]
pub fn one_liner(invoked_name: &str) -> String {
    format!(
        "Stuck? This request_id and the error code may be sent to the \
         operator on your next successful command — nothing else, none of \
         your own data. To disable: {invoked_name} config set phone-home off"
    )
}

/// Record a failure. Appends to the rolling failure journal (burst
/// detection), and — when the failure is an abandonment predictor and
/// phone-home is enabled — queues a [`PendingReport`] and returns the
/// one-liner to print. Best-effort throughout: IO problems silently
/// drop the signal, never the command's own error.
pub fn on_failure(
    dir: &Path,
    host: &str,
    enabled: bool,
    invoked_name: &str,
    err: &Error,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let Error::Api { status, body } = err else {
        return None;
    };

    // Roll the journal: prune outside the burst window, append this one.
    let jpath = journal_path(dir);
    let mut journal: Vec<JournalEntry> = read_jsonl(&jpath)
        .into_iter()
        .filter(|e: &JournalEntry| {
            e.host == host && (now - e.at).num_seconds() <= BURST_WINDOW_SECS
        })
        .collect();
    journal.push(JournalEntry {
        host: host.to_owned(),
        status: *status,
        at: now,
    });
    let recent_4xx = journal
        .iter()
        .filter(|e| (400..500).contains(&e.status))
        .count();
    write_jsonl(&jpath, &journal);

    if !is_abandon_predictor(*status, recent_4xx) {
        return None;
    }

    // Queue exactly the promised pair: request_id + stable code.
    let request_id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .and_then(sanitize_token)
        .unwrap_or_default();
    let label = body
        .get("error")
        .and_then(|v| v.as_str())
        .and_then(sanitize_token)
        .unwrap_or_default();
    let code = sanitize_token(&format!("{status} {label}"))?;

    let ppath = pending_path(dir);
    let mut pending: Vec<PendingReport> = read_jsonl(&ppath);
    pending.push(PendingReport {
        host: host.to_owned(),
        request_id,
        code,
        at: now.to_rfc3339(),
    });
    // Cap the spool — keep the most recent reports only.
    if pending.len() > MAX_REPORTS_PER_HEADER * 4 {
        let drop = pending.len() - MAX_REPORTS_PER_HEADER * 4;
        pending.drain(..drop);
    }
    write_jsonl(&ppath, &pending);

    Some(one_liner(invoked_name))
}

/// Drain pending reports for `host` into a header value
/// (`rid:code,rid:code`, ≤ [`MAX_REPORTS_PER_HEADER`] entries).
/// Entries for other hosts stay queued. Returns `None` when there is
/// nothing to send. The drain is destructive by design: a report
/// rides at most one request.
pub fn drain_for_header(dir: &Path, host: &str) -> Option<String> {
    let ppath = pending_path(dir);
    let pending: Vec<PendingReport> = read_jsonl(&ppath);
    if pending.is_empty() {
        return None;
    }
    let (mine, others): (Vec<_>, Vec<_>) = pending.into_iter().partition(|r| r.host == host);
    if mine.is_empty() {
        return None;
    }
    let header = mine
        .iter()
        .take(MAX_REPORTS_PER_HEADER)
        .map(|r| {
            if r.request_id.is_empty() {
                format!("-:{}", r.code)
            } else {
                format!("{}:{}", r.request_id, r.code)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    write_jsonl(&ppath, &others);
    Some(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::TempDir;

    const HOST: &str = "https://avatar.example";

    fn api_err(status: u16, label: &str, request_id: &str) -> Error {
        Error::Api {
            status,
            body: serde_json::json!({"error": label, "request_id": request_id}),
        }
    }

    #[test]
    fn predictor_classes() {
        assert!(is_abandon_predictor(503, 0));
        assert!(is_abandon_predictor(502, 0));
        assert!(!is_abandon_predictor(404, 1));
        assert!(!is_abandon_predictor(409, 2));
        assert!(is_abandon_predictor(409, 3));
        assert!(!is_abandon_predictor(500, 9));
    }

    #[test]
    fn five03_queues_report_and_returns_line() {
        let td = TempDir::new().unwrap();
        let line = on_failure(
            td.path(),
            HOST,
            true,
            "cosmon-remote",
            &api_err(503, "tackle_unavailable", "req-abc123"),
            Utc::now(),
        );
        assert!(line.is_some());
        assert!(line.unwrap().contains("config set phone-home off"));
        let header = drain_for_header(td.path(), HOST).unwrap();
        assert_eq!(header, "req-abc123:503_tackle_unavailable");
        // Drained — a report rides at most one request.
        assert!(drain_for_header(td.path(), HOST).is_none());
    }

    #[test]
    fn fourxx_burst_only_fires_at_third() {
        let td = TempDir::new().unwrap();
        let now = Utc::now();
        let err = api_err(409, "reserved_name", "req-x1");
        assert!(on_failure(td.path(), HOST, true, "cosmon-remote", &err, now).is_none());
        assert!(on_failure(td.path(), HOST, true, "cosmon-remote", &err, now).is_none());
        assert!(on_failure(td.path(), HOST, true, "cosmon-remote", &err, now).is_some());
    }

    // Gate: opt-out effectif — after the gesture, no queue write and
    // no header, even with reports already pending.
    #[test]
    fn opt_out_stops_all_remontee() {
        let td = TempDir::new().unwrap();
        // While enabled: one report queued.
        let _ = on_failure(
            td.path(),
            HOST,
            true,
            "cosmon-remote",
            &api_err(503, "tackle_unavailable", "req-1"),
            Utc::now(),
        );
        // The gesture: phone-home off. New failures produce nothing.
        let line = on_failure(
            td.path(),
            HOST,
            false,
            "cosmon-remote",
            &api_err(503, "tackle_unavailable", "req-2"),
            Utc::now(),
        );
        assert!(line.is_none());
        // The pending file still has only the pre-gesture report; the
        // caller (client) also gates the drain on the same flag, so
        // nothing rides — asserted at the client level too.
        let pending: Vec<PendingReport> = read_jsonl(&pending_path(td.path()));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, "req-1");
    }

    // Gate: anti-fuite — the spool and the header never carry artifact
    // content or a raw sub, whatever the server (or an attacker)
    // echoes in the error body.
    #[test]
    fn no_tenant_content_ever_queued() {
        let td = TempDir::new().unwrap();
        let raw_sub = "tenant-demo-operator";
        let secret_artifact = "synthesis.md: la stratégie confidentielle du client";
        let err = Error::Api {
            status: 503,
            body: serde_json::json!({
                "error": "tackle_unavailable",
                "request_id": "req-leak1",
                "sub": raw_sub,
                "artifact_body": secret_artifact,
                "detail": {"path": "/casier/prive/notes.md"},
            }),
        };
        let _ = on_failure(td.path(), HOST, true, "cosmon-remote", &err, Utc::now());
        let spool = std::fs::read_to_string(pending_path(td.path())).unwrap();
        assert!(!spool.contains(raw_sub));
        assert!(!spool.contains("confidentielle"));
        assert!(!spool.contains("casier"));
        let header = drain_for_header(td.path(), HOST).unwrap();
        assert!(!header.contains(raw_sub));
        assert!(!header.contains("confidentielle"));
        assert_eq!(header, "req-leak1:503_tackle_unavailable");
    }

    #[test]
    fn sanitize_strips_hostiles() {
        assert_eq!(
            sanitize_token("../../etc/passwd").as_deref(),
            Some("....etcpasswd")
        );
        assert_eq!(sanitize_token("réq id").as_deref(), Some("rq_id"));
        assert!(sanitize_token("///").is_none());
        assert!(sanitize_token("...").is_none());
        assert!(sanitize_token("").is_none());
        let long = "a".repeat(200);
        assert_eq!(sanitize_token(&long).unwrap().len(), 64);
    }

    #[test]
    fn reports_stay_with_their_host() {
        let td = TempDir::new().unwrap();
        let _ = on_failure(
            td.path(),
            "https://a.example",
            true,
            "cosmon-remote",
            &api_err(503, "x", "req-a"),
            Utc::now(),
        );
        let _ = on_failure(
            td.path(),
            "https://b.example",
            true,
            "cosmon-remote",
            &api_err(503, "y", "req-b"),
            Utc::now(),
        );
        let ha = drain_for_header(td.path(), "https://a.example").unwrap();
        assert!(ha.starts_with("req-a:"));
        // b's report is still queued for b.
        let hb = drain_for_header(td.path(), "https://b.example").unwrap();
        assert!(hb.starts_with("req-b:"));
    }

    #[test]
    fn non_api_errors_are_ignored() {
        let td = TempDir::new().unwrap();
        let err = Error::Config("missing field".into());
        assert!(on_failure(td.path(), HOST, true, "cosmon-remote", &err, Utc::now()).is_none());
    }
}
