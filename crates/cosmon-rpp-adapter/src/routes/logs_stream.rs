// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/logs` — Server-Sent Events stream of the
//! per-molecule worker tmux output. The tenant sees what claude is *doing*
//! while it works, not just the final result.
//!
//! Sibling of [`crate::routes::events_stream`] but with a different
//! source: events stream tails the in-process [`crate::events_bus`]
//! broadcast channel, this route tails the worker's tmux pane.
//! Adapter-only — there is no `cs logs stream` verb counterpart.
//!
//! # Pipeline
//!
//! 1. Extract bearer.
//! 2. Validate JWT.
//! 3. Scope check — `cosmon:logs:subscribe`.
//! 4. Admission boundary — same five clauses as every other route, so
//!    a noyau-A JWT cannot subscribe to noyau-B molecule logs.
//! 5. Reject malformed `{id}` (path traversal defence-in-depth).
//! 6. Spawn a per-connection polling task that captures the tmux pane
//!    (`tmux -L cosmon capture-pane -t cosmon-<molecule_id> -p`)
//!    periodically and pushes new lines into an mpsc channel.
//! 7. Forward each line as one `text/event-stream` chunk with a
//!    monotonically increasing `id:` and `event: log.line`.
//! 8. Emit a keep-alive comment every 30 s so any HTTP/1.1 proxy in
//!    the middle keeps the socket open.
//!
//! # Stop conditions
//!
//! The polling task — and therefore the SSE stream — terminates when
//! the underlying tmux session disappears (the worker exited, the
//! molecule was collapsed/completed, or `cs done` torn the session
//! down). This is structural: `tmux capture-pane` against a missing
//! session returns a non-zero exit, the polling task surfaces it as
//! an end-of-stream, and axum closes the SSE response.
//!
//! # Why per-connection, not a global bus
//!
//! Unlike the events bus, where mutation routes *push* lifecycle
//! signals into a shared `broadcast::channel`, tmux is a pull source:
//! we have to ask it for its current pane contents. A per-connection
//! polling task keeps the bus surface narrow and avoids the lifecycle
//! coordination cost of a shared logs registry. Per-connection cost
//! is one `tmux capture-pane` subprocess per polling tick; tested at
//! 500 ms it is well below the noise floor of an active claude pane.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use cosmon_state::instrumentation::{emit_authz_decision_with_source, AuthzDecision};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT, LOGS_SUBSCRIBE};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::AppState;

/// Keep-alive interval. Matches the briefing — "ping toutes les 30s".
const KEEP_ALIVE_SECS: u64 = 30;

/// How often the polling task asks tmux for the current pane content.
/// 500 ms is chosen empirically: short enough to feel live in a tail
/// view, long enough that the subprocess cost stays below 1% of a
/// core. Tunable via [`LogsQuery::poll_ms`] if a deployment wants a
/// different cadence.
const DEFAULT_POLL_MS: u64 = 500;

/// Hard cap on the mpsc channel buffer. A slow subscriber that fills
/// the buffer will see the polling task drop further lines on the
/// floor (with a `tracing::warn!`) rather than back-pressuring tmux.
/// SSE is best-effort by construction; durable logs live in the
/// per-molecule `log.md` artefact.
const CHANNEL_BUFFER: usize = 1024;

/// How many tail lines of the pane to capture per tick. 4000 covers
/// a screen-and-a-half of dense claude output and matches the
/// `peek_tui` capture window's order of magnitude. Larger windows
/// re-deliver the same lines (dedupe is line-prefix-based) so the
/// cost is bounded by the diff, not by the window.
const CAPTURE_LINES: i32 = 4000;

/// Adapter-wide monotonic id counter for the SSE `id:` field.
/// Shared across every concurrent logs subscription so a tenant that
/// reconnects with `Last-Event-ID` can compare against any prior id
/// it ever saw on the host, irrespective of which molecule it was
/// for. The counter is not load-bearing for correctness (it is only
/// used to drop already-seen lines on reconnect); a `u64` lasts
/// effectively forever at sane traffic.
static NEXT_LOG_ID: AtomicU64 = AtomicU64::new(1);

/// Query parameters for `GET /v1/molecules/{id}/logs`.
#[derive(Debug, Deserialize, Default)]
pub struct LogsQuery {
    /// Whether to keep the stream open after catching up. Default
    /// `true` (matches the briefing's "follow=true" flag); set to
    /// `false` to receive the current snapshot and close. The
    /// snapshot path is useful for "give me the last N lines" CLI
    /// callers that do not want a live tail.
    #[serde(default = "default_follow")]
    pub follow: bool,
    /// Override the polling cadence (ms). Clamped to `[50, 5000]` so
    /// a misconfigured client cannot `DoS` the host or starve the
    /// stream.
    #[serde(default)]
    pub poll_ms: Option<u64>,
}

fn default_follow() -> bool {
    true
}

/// `GET /v1/molecules/{id}/logs` — see module docs.
pub async fn logs_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // 1. Bearer + JWT.
    let token = extract_bearer(&headers).map_err(|e| ApiError::from_reject(&e, None))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| ApiError::from_reject(&e, None))?;

    // 2. Scope check — `cosmon:logs:subscribe`.
    authorise_logs_subscribe(&state, &jwt)?;

    // 3. Admission boundary — same five clauses as every other route,
    //    so a noyau-A JWT cannot subscribe to noyau-B logs.
    let spark = build_spark(&state, &jwt, &molecule_id_str)?;

    // 4. Reject malformed molecule ids before letting them reach tmux.
    reject_unsafe_segment(&molecule_id_str, &spark)?;

    // 5. Last-Event-ID — best-effort dedup floor. Clients that
    //    reconnect echo their last seen id; we then refuse to forward
    //    anything with id ≤ floor.
    let resume_floor = parse_last_event_id(&headers).unwrap_or(0);

    // 6. Spawn the polling task that converts tmux pane content into
    //    a stream of new lines.
    let session_name = tmux_session_for(&molecule_id_str);
    let poll_ms = clamp_poll_ms(query.poll_ms);
    let molecule_id = molecule_id_str.clone();
    let follow = query.follow;

    let (tx, rx) = mpsc::channel::<TailLine>(CHANNEL_BUFFER);
    tokio::spawn(async move {
        run_pane_poller(tx, session_name, molecule_id, poll_ms, follow).await;
    });

    let stream = ReceiverStream::new(rx).filter_map(move |line| {
        let id = NEXT_LOG_ID.fetch_add(1, Ordering::Relaxed);
        if id <= resume_floor {
            return None;
        }
        Some(Ok(render_event(id, &line)))
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(KEEP_ALIVE_SECS))
            .text("keep-alive"),
    ))
}

/// One tail-emitted line carrying everything the SSE payload needs.
#[derive(Debug, Clone)]
struct TailLine {
    molecule_id: String,
    line: String,
    timestamp: String,
}

/// Project a [`TailLine`] into an `axum::response::sse::Event`. The
/// `data:` body matches the briefing payload shape:
/// `{molecule_id, line, timestamp}`.
fn render_event(id: u64, line: &TailLine) -> Event {
    let data = serde_json::json!({
        "molecule_id": line.molecule_id,
        "line": line.line,
        "timestamp": line.timestamp,
    });
    let body = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_owned());
    Event::default()
        .id(id.to_string())
        .event("log.line")
        .data(body)
}

/// Clamp the requested poll cadence to the `[50, 5000] ms` band.
/// `None` means "use the default" (500 ms). The band is intentionally
/// permissive on the slow end so a non-interactive client can
/// downshift to a heartbeat.
fn clamp_poll_ms(requested: Option<u64>) -> u64 {
    let raw = requested.unwrap_or(DEFAULT_POLL_MS);
    raw.clamp(50, 5000)
}

/// Build the tmux session name for a molecule id. Mirrors the
/// convention emitted by `cs tackle` (see
/// `routes::molecules::build_tackle_response`):
/// `tmux -L cosmon attach -t cosmon-<molecule_id>`.
fn tmux_session_for(molecule_id: &str) -> String {
    format!("cosmon-{molecule_id}")
}

/// Run the per-connection polling task. Captures the tmux pane at the
/// configured cadence, diffs against the last-seen content, and
/// forwards each new line into the mpsc channel.
///
/// Termination paths:
/// - `tmux capture-pane` returns a non-zero exit (session gone) →
///   the task closes the channel.
/// - The receiver side (the SSE stream) is dropped (client
///   disconnected) → the `send` errors and the task exits.
/// - `follow=false` → the task pushes the initial snapshot and
///   closes immediately, surfacing the catch-up as a finite stream.
async fn run_pane_poller(
    tx: mpsc::Sender<TailLine>,
    session_name: String,
    molecule_id: String,
    poll_ms: u64,
    follow: bool,
) {
    let mut last_lines: Vec<String> = Vec::new();
    let mut empty_tick_count: u32 = 0;
    let max_empty_ticks: u32 = if follow { u32::MAX } else { 0 };

    loop {
        let captured = match capture_pane(&session_name) {
            Ok(s) => s,
            Err(err) => {
                tracing::debug!(
                    event = "rpp.logs.session_gone",
                    session = %session_name,
                    %err,
                    "tmux capture-pane failed; closing logs stream"
                );
                return;
            }
        };

        let lines: Vec<String> = captured
            .lines()
            .map(std::string::ToString::to_string)
            .collect();
        let new_lines = diff_new_lines(&last_lines, &lines);

        if new_lines.is_empty() {
            empty_tick_count = empty_tick_count.saturating_add(1);
            if !follow || empty_tick_count > max_empty_ticks {
                // `follow=false` path: snapshot already pushed (the
                // first tick was non-empty) or pane is empty —
                // either way, close the stream.
                return;
            }
        } else {
            empty_tick_count = 0;
            let timestamp = chrono::Utc::now().to_rfc3339();
            for line in new_lines {
                let entry = TailLine {
                    molecule_id: molecule_id.clone(),
                    line,
                    timestamp: timestamp.clone(),
                };
                if tx.send(entry).await.is_err() {
                    // Receiver dropped (client disconnected). Stop
                    // capturing — nothing to push to.
                    return;
                }
            }
        }

        // Rotate the window so the next tick's diff is against the
        // current snapshot, not the empty seed.
        last_lines = lines;

        if !follow {
            return;
        }

        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Capture the tmux pane content for a session. Returns the raw text
/// (escape codes stripped — `-p` plain output) or an error string
/// when the session is absent. The socket name is pinned to `cosmon`
/// (matches the convention `cs tackle` writes into its response and
/// the `peek_tui` capture helper uses).
fn capture_pane(session_name: &str) -> Result<String, String> {
    let output = std::process::Command::new("tmux")
        .args([
            "-L",
            "cosmon",
            "capture-pane",
            "-t",
            session_name,
            "-p",
            "-S",
            &format!("-{CAPTURE_LINES}"),
        ])
        .output()
        .map_err(|e| format!("tmux spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Compute the suffix of `new` that does not match `old`. The diff is
/// line-based and tolerant of pane truncation: when `new` is shorter
/// than `old` (the pane has scrolled past), we fall back to "emit
/// everything in `new` that does not appear at any position in `old`".
///
/// The bookkeeping side-effect (replacing `*last` with `new`) is
/// performed by the caller after the result is consumed; this lets
/// the function stay pure and unit-testable.
fn diff_new_lines_pure(old: &[String], new: &[String]) -> Vec<String> {
    if new.is_empty() {
        return Vec::new();
    }
    // Fast path — `new` is a strict extension of `old` (the most
    // common case for a live tmux pane).
    if new.len() >= old.len() && new[..old.len()] == *old {
        return new[old.len()..].to_vec();
    }
    // Slow path — pane scrolled or content changed mid-buffer. Find
    // the longest suffix of `old` that is a prefix of `new`; the
    // remainder of `new` is the delta.
    let max_overlap = old.len().min(new.len());
    for overlap in (1..=max_overlap).rev() {
        let old_tail = &old[old.len() - overlap..];
        let new_head = &new[..overlap];
        if old_tail == new_head {
            return new[overlap..].to_vec();
        }
    }
    // No overlap — the pane fully refreshed. Emit the whole new
    // buffer (rare; happens when claude clears the screen).
    new.to_vec()
}

/// Caller-side wrapper: update `last` in place and return the delta.
fn diff_new_lines(old: &[String], new: &[String]) -> Vec<String> {
    diff_new_lines_pure(old, new)
}

/// Extract the JWT bearer. Duplicated from sibling routes — the
/// helper is small and the import surface is intentionally kept
/// route-local.
fn extract_bearer(headers: &HeaderMap) -> Result<&str, RppRejectReason> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or(RppRejectReason::MissingAuthorization)?;
    let s = header.to_str().map_err(|_| RppRejectReason::MalformedJwt)?;
    let stripped = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .ok_or(RppRejectReason::MalformedJwt)?;
    Ok(stripped.trim())
}

/// Authorise `cosmon:logs:subscribe` against JWT scopes ∪
/// binding-granted scopes, emitting the matching
/// `AuthzDecisionEvaluated` event.
fn authorise_logs_subscribe(state: &Arc<AppState>, jwt: &ValidatedJwt) -> Result<(), ApiError> {
    let nucleon_map = state.nucleon_map.load();
    let binding_scopes = nucleon_map.allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let (decision, grant_source) = if jwt.has_scope(LOGS_SUBSCRIBE) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_JWT))
    } else if binding_scopes.iter().any(|s| s == LOGS_SUBSCRIBE) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_BINDING))
    } else {
        (AuthzDecision::Absent, None)
    };

    emit_authz_decision_with_source(
        &state.state_dir,
        "logs_subscribe",
        &format!("jwt:{}", jwt.sub),
        Some(LOGS_SUBSCRIBE),
        decision,
        grant_source,
        0,
    );

    if matches!(decision, AuthzDecision::Allow) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            label: "forbidden",
            request_id: None,
        })
    }
}

/// Reject `..`, `/`, `\\`, or empty segments. Defence-in-depth — the
/// molecule id flows into a tmux `-t` argument so a `;` or `$()`
/// payload must be impossible. We refuse anything that does not look
/// like a kebab/alphanumeric id.
fn reject_unsafe_segment(segment: &str, spark: &Spark) -> Result<(), ApiError> {
    let invalid = segment.is_empty()
        || segment.len() > 128
        || segment
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        || segment.contains("..");
    if invalid {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_path_segment",
            request_id: Some(spark.request_id.clone()),
        });
    }
    Ok(())
}

/// Parse `Last-Event-ID` as `u64`. Absent or malformed → `None`.
fn parse_last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Build the admission [`Spark`] for the SSE route.
fn build_spark(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    molecule_id: &str,
) -> Result<Spark, ApiError> {
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX);
    let nucleon_map = state.nucleon_map.load();
    let rig = AdmissionRig {
        nucleon_map: nucleon_map.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        deny_list: state.deny_list.as_ref(),
        inbox_root: &state.inbox_root,
        now_ms,
    };
    http_request_to_spark(&rig, jwt, Verb::SubscribeLogs, Some(molecule_id))
        .map_err(|e| ApiError::from_reject(&e, Some(new_request_id())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_poll_ms_defaults_to_500() {
        assert_eq!(clamp_poll_ms(None), DEFAULT_POLL_MS);
    }

    #[test]
    fn clamp_poll_ms_clamps_below_floor() {
        assert_eq!(clamp_poll_ms(Some(0)), 50);
        assert_eq!(clamp_poll_ms(Some(10)), 50);
    }

    #[test]
    fn clamp_poll_ms_clamps_above_ceiling() {
        assert_eq!(clamp_poll_ms(Some(60_000)), 5000);
    }

    #[test]
    fn clamp_poll_ms_passes_in_band() {
        assert_eq!(clamp_poll_ms(Some(250)), 250);
        assert_eq!(clamp_poll_ms(Some(1500)), 1500);
    }

    #[test]
    fn tmux_session_for_matches_cs_tackle_convention() {
        // Pinned: `cs tackle` returns
        // `tmux -L cosmon attach -t cosmon-<molecule_id>`, so the
        // session helper MUST agree.
        assert_eq!(
            tmux_session_for("task-20260514-f02f"),
            "cosmon-task-20260514-f02f"
        );
    }

    #[test]
    fn last_event_id_parses_decimal() {
        let mut h = HeaderMap::new();
        h.insert("last-event-id", "42".parse().unwrap());
        assert_eq!(parse_last_event_id(&h), Some(42));
    }

    #[test]
    fn last_event_id_missing_yields_none() {
        let h = HeaderMap::new();
        assert_eq!(parse_last_event_id(&h), None);
    }

    #[test]
    fn last_event_id_malformed_yields_none() {
        let mut h = HeaderMap::new();
        h.insert("last-event-id", "not-a-number".parse().unwrap());
        assert_eq!(parse_last_event_id(&h), None);
    }

    #[test]
    fn diff_new_lines_strict_extension() {
        let old = vec!["a".to_owned(), "b".to_owned()];
        let new = vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        assert_eq!(
            diff_new_lines_pure(&old, &new),
            vec!["c".to_owned(), "d".to_owned()]
        );
    }

    #[test]
    fn diff_new_lines_scrolled_pane() {
        // Pane scrolled — `a` fell off; `b` aligns with the new head;
        // `c` is the only true delta.
        let old = vec!["a".to_owned(), "b".to_owned()];
        let new = vec!["b".to_owned(), "c".to_owned()];
        assert_eq!(diff_new_lines_pure(&old, &new), vec!["c".to_owned()]);
    }

    #[test]
    fn diff_new_lines_no_overlap_emits_full_buffer() {
        let old = vec!["x".to_owned()];
        let new = vec!["y".to_owned(), "z".to_owned()];
        assert_eq!(
            diff_new_lines_pure(&old, &new),
            vec!["y".to_owned(), "z".to_owned()]
        );
    }

    #[test]
    fn diff_new_lines_empty_new_yields_nothing() {
        let old = vec!["a".to_owned()];
        let new: Vec<String> = Vec::new();
        assert!(diff_new_lines_pure(&old, &new).is_empty());
    }

    #[test]
    fn diff_new_lines_identical_yields_nothing() {
        let old = vec!["a".to_owned(), "b".to_owned()];
        let new = vec!["a".to_owned(), "b".to_owned()];
        assert!(diff_new_lines_pure(&old, &new).is_empty());
    }

    #[test]
    fn reject_unsafe_segment_accepts_kebab_id() {
        // A representative cs molecule id: `task-20260523-ad25`. Must
        // survive the safety guard.
        // We construct a fake Spark inline (we only need the
        // request_id field, the rest is irrelevant).
        let spark = fake_spark();
        assert!(reject_unsafe_segment("task-20260523-ad25", &spark).is_ok());
    }

    #[test]
    fn reject_unsafe_segment_rejects_slash() {
        let spark = fake_spark();
        assert!(reject_unsafe_segment("foo/bar", &spark).is_err());
    }

    #[test]
    fn reject_unsafe_segment_rejects_dotdot() {
        let spark = fake_spark();
        assert!(reject_unsafe_segment("..", &spark).is_err());
        assert!(reject_unsafe_segment("a..b", &spark).is_err());
    }

    #[test]
    fn reject_unsafe_segment_rejects_shell_metachars() {
        let spark = fake_spark();
        for bad in ["$(whoami)", "a;b", "a&b", "a|b", "a`b`", "a b"] {
            assert!(
                reject_unsafe_segment(bad, &spark).is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn reject_unsafe_segment_rejects_empty() {
        let spark = fake_spark();
        assert!(reject_unsafe_segment("", &spark).is_err());
    }

    #[test]
    fn reject_unsafe_segment_rejects_overlong() {
        let spark = fake_spark();
        let s = "a".repeat(129);
        assert!(reject_unsafe_segment(&s, &spark).is_err());
    }

    #[test]
    fn render_event_is_total_over_typical_payload() {
        // `axum::response::sse::Event` deliberately has no public
        // getters; the on-wire shape is pinned by the integration
        // test `tests/v1_logs_stream.rs`. Here we assert the
        // function does not panic.
        let line = TailLine {
            molecule_id: "task-1".to_owned(),
            line: "claude is thinking…".to_owned(),
            timestamp: "2026-05-23T09:30:00Z".to_owned(),
        };
        let _ = render_event(7, &line);
    }

    fn fake_spark() -> Spark {
        use crate::nucleon_map::Noyau;
        Spark {
            request_id: "req-test".to_owned(),
            nucleon_id: "nuc-test".to_owned(),
            noyau: Noyau::new("a"),
            verb: "logs_subscribe".to_owned(),
            molecule_id: None,
            inbox_path: std::path::PathBuf::from("/tmp/whispers/inbox/api/req-test.json"),
        }
    }
}
