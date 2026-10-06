// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/ledger` — replay, then live, over the tenant's durable event log.
//!
//! `GET /v1/events` is a live tail of an in-process bus: it replays nothing,
//! only sees what the adapter's own handlers publish, and its ids restart
//! with the process. A consumer that must not miss a lifecycle event needs
//! the durable record instead. This route serves it: one reader over the
//! tenant's ledger reads to the end and then keeps following the same
//! position, so replay and live share one position and no line can fall
//! between them or be delivered twice. Lines appended by workers, which
//! never pass through the adapter, are in the ledger and therefore here.
//!
//! # Cursor
//!
//! The resume cursor is the SSE `id` of the last frame received, an opaque
//! token. It is not the envelope `seq`: lifecycle lines written by the file
//! store carry no `seq`, and where `seq` exists it is not monotone. A
//! client resumes with `Last-Event-ID` (what an `EventSource` sends on its
//! own) or `?after=`; the header wins when both are present, because a
//! reconnecting client replays its original URL. With neither, the stream
//! starts at the beginning of the ledger.
//!
//! The first frame is always `ledger.epoch`. A cursor that cannot be
//! honoured (another log, a truncated log, a malformed token) and a log
//! replaced while the stream is open are both announced by a `ledger.reset`
//! frame, after which the stream replays from the start of the current log.
//! The client drops what it folded and starts over. Nothing is skipped
//! without a frame saying so.
//!
//! # What is served
//!
//! Never the raw line. Each line becomes a projection of the documented
//! fields of its type ([`PROJECTIONS`], mirrored in
//! `docs/book/src/reference/read-contracts.md`):
//!
//! - lines of a type outside the table are dropped, so a field added to a
//!   new event type is not disclosed until it is documented;
//! - lines that name no molecule are dropped (`operator_present`,
//!   `operator_signed`, which are about 46 % of the ledger, and the
//!   worker-only lines), because this is a molecule-scoped stream;
//! - `session_id` is replaced by a digest; worktree paths, shell commands
//!   and harness-turn text are not in any projection.
//!
//! `molecule_step_completed.evidence` is free text a worker wrote. It is a
//! disclosure class no other route serves, which is why the route requires
//! `cosmon:events:subscribe` **and** `cosmon:molecule:read`.
//!
//! # Bounds
//!
//! A connection delivers at most [`FRAME_CAP`] frames, then closes cleanly;
//! the client reconnects from its last id. At most one ledger stream is open
//! per principal. Admission is rechecked every second by the shared stream
//! guard, as on the other two streams.

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use cosmon_state::event_log::{LedgerRead, LedgerReader};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{EVENTS_SUBSCRIBE, MOLECULE_READ, MOLECULE_WRITE};
use crate::error::ApiError;
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::routes::molecules::{authorise_scope_public, extract_bearer};
use crate::routes::stream_guard::guard_stream;
use crate::AppState;

/// Frames delivered per connection before it closes and the client resumes.
pub const FRAME_CAP: usize = 5_000;

/// Lines pulled from the ledger per read. Bounds memory per poll, not the
/// number of lines a connection may scan.
const READ_BATCH: usize = 500;

/// Pause between polls once the reader has reached the end of the ledger.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Keep-alive comment interval, as on the other streams.
const KEEP_ALIVE_SECS: u64 = 30;

/// Query parameters for `GET /v1/ledger`.
#[derive(Debug, Deserialize, Default)]
pub struct LedgerQuery {
    /// Resume cursor: the `id` of the last frame the client received.
    /// `Last-Event-ID` takes precedence when both are sent.
    #[serde(default)]
    pub after: Option<String>,
}

/// Documented fields per event type: the whole of what a frame can carry
/// beyond the envelope keys (`timestamp`, `seq`, `mol_seq`). Adding a type here is a
/// contract change and updates `read-contracts.md` in the same commit.
pub const PROJECTIONS: &[(&str, &[&str])] = &[
    ("molecule_nucleated", &["formula_id", "blocks"]),
    ("molecule_status_changed", &["from", "to"]),
    ("molecule_transitioned", &["from", "to"]),
    ("molecule_evolved", &["step", "total"]),
    ("molecule_step_completed", &["step", "total", "evidence"]),
    ("molecule_completed", &["reason", "summary"]),
    ("molecule_collapsed", &["reason", "kind"]),
    ("molecule_frozen", &[]),
    ("molecule_thawed", &[]),
    ("merge_completed", &["result", "branch"]),
    ("harvested", &["success"]),
    ("worker_spawned", &["worker_id", "adapter_name"]),
    ("adapter_selected", &["worker_id", "adapter_name"]),
    ("model_selected", &["worker_id", "adapter_name", "model"]),
    ("model_observed", &["worker_id", "adapter_name", "model"]),
    (
        "session_presence",
        &["provider", "role", "worker_id", "state", "detail", "ts"],
    ),
];

/// Keys every frame may carry in addition to its type's fields.
const ENVELOPE_KEYS: &[&str] = &["timestamp", "seq", "mol_seq"];

/// The three spellings of the molecule key in the ledger, canonical first.
const MOLECULE_KEYS: &[&str] = &["molecule_id", "mol_id", "molecule"];

/// Project one raw ledger line into `(event name, frame data)`, or `None`
/// when the line must not be served. Total over arbitrary input.
#[must_use]
pub fn project_line(raw: &str) -> Option<(String, Value)> {
    let line: Value = serde_json::from_str(raw).ok()?;
    let obj = line.as_object()?;

    // Current lines tag the event `type`; the file store's older lines tag
    // it `kind`. When there is no `type`, `kind` is the tag, not a field.
    let (event, kind_is_field) = match obj.get("type").and_then(Value::as_str) {
        Some(t) => (t, true),
        None => (obj.get("kind").and_then(Value::as_str)?, false),
    };
    let (_, fields) = PROJECTIONS.iter().find(|(name, _)| *name == event)?;
    let molecule_id = MOLECULE_KEYS
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))?;

    let mut frame = Map::new();
    frame.insert(
        "schema_version".to_owned(),
        obj.get("schema_version")
            .cloned()
            .unwrap_or_else(|| json!(1)),
    );
    frame.insert("type".to_owned(), json!(event));
    frame.insert("molecule_id".to_owned(), json!(molecule_id));
    for key in ENVELOPE_KEYS.iter().chain(fields.iter()) {
        if *key == "kind" && !kind_is_field {
            continue;
        }
        if let Some(value) = obj.get(*key) {
            frame.insert((*key).to_owned(), value.clone());
        }
    }
    if event == "session_presence" {
        if let Some(id) = obj.get("session_id").and_then(Value::as_str) {
            frame.insert("session_digest".to_owned(), json!(digest(id)));
        }
    }
    Some((event.to_owned(), Value::Object(frame)))
}

/// Short stable digest: lets a consumer tell sessions apart without being
/// handed the harness identifier.
fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .take(8)
        .fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Principals that currently hold a ledger stream.
fn open_streams() -> &'static Mutex<HashSet<String>> {
    static OPEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Holds a principal's single ledger-stream slot until dropped.
struct StreamSlot(String);

impl StreamSlot {
    fn acquire(principal: String) -> Option<Self> {
        let mut open = open_streams()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Lazily: building a slot that is then discarded would release
        // the principal's real slot (and deadlock on `open`).
        open.insert(principal.clone()).then(|| Self(principal))
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        open_streams()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// `GET /v1/ledger` — see module docs.
pub async fn ledger_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LedgerQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // 1. Bearer + JWT.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 2. Both scopes: the stream tail is one grant, the free-text evidence
    //    it carries is molecule data.
    authorise_scope_public(
        &state,
        &jwt,
        "ledger_subscribe",
        &[EVENTS_SUBSCRIBE],
        EVENTS_SUBSCRIBE,
    )?;
    authorise_scope_public(
        &state,
        &jwt,
        "ledger_subscribe",
        &[MOLECULE_READ, MOLECULE_WRITE],
        MOLECULE_READ,
    )?;

    // 3. Admission boundary: pins the tenant.
    let spark = build_spark(&state, &jwt)?;

    // 4. The tenant's ledger, resolved through the state crate.
    let tenant_root = state.galaxies_root.join(spark.noyau.as_str());
    if !tenant_root.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            label: "not_found",
            request_id: Some(spark.request_id.clone()),
        });
    }
    let ledger_path = cosmon_state::event_log::resolve_events_log_path(
        &tenant_root.join(".cosmon").join("state"),
    );

    // 5. One stream per principal.
    let slot = StreamSlot::acquire(crate::rate_limit::hash_principal(&jwt.iss, &jwt.sub))
        .ok_or_else(|| ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            label: "ledger_stream_open",
            request_id: Some(spark.request_id.clone()),
        })?;

    // 6. Position: the header (a reconnect) wins over the query.
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or(query.after);
    let (reader, reset) =
        LedgerReader::open(&ledger_path, after.as_deref()).map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "store_unavailable",
            request_id: Some(spark.request_id.clone()),
        })?;

    // 7. One task owns the reader; the channel is the
    //    back-pressure and its closing is how the task learns the client
    //    (or the admission guard) is gone.
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
    let mut first = vec![epoch_frame(&reader)];
    if let Some(reason) = reset {
        first.push(reset_frame(reason.as_str(), &reader));
    }
    tokio::spawn(pump(reader, first, tx));

    // The slot lives in the stream, so it is released the moment the
    // response body is dropped, not when the task next wakes.
    let stream = ReceiverStream::new(rx).map(move |frame| {
        let _held = &slot;
        Ok(frame)
    });
    let stream = guard_stream(
        stream,
        state,
        token.to_owned(),
        spark,
        EVENTS_SUBSCRIBE,
        "ledger",
    );
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(KEEP_ALIVE_SECS))
            .text("keep-alive"),
    ))
}

/// `ledger.epoch`: names the log the cursors of this connection refer to.
fn epoch_frame(reader: &LedgerReader) -> Event {
    let cursor = reader.cursor();
    Event::default()
        .id(cursor.to_string())
        .event("ledger.epoch")
        .data(json!({"epoch": cursor.epoch, "cursor": cursor.to_string()}).to_string())
}

/// `ledger.reset`: the consumer's folded state is stale; the stream restarts
/// from the beginning of the log named by the cursor.
fn reset_frame(reason: &str, reader: &LedgerReader) -> Event {
    let cursor = reader.cursor();
    Event::default()
        .id(cursor.to_string())
        .event("ledger.reset")
        .data(
            json!({"reason": reason, "epoch": cursor.epoch, "cursor": cursor.to_string()})
                .to_string(),
        )
}

/// Read the ledger and push frames until the client leaves, the cap is
/// reached, or the log cannot be read.
async fn pump(mut reader: LedgerReader, first: Vec<Event>, tx: tokio::sync::mpsc::Sender<Event>) {
    let mut sent = 0usize;
    for frame in first {
        if tx.send(frame).await.is_err() {
            return;
        }
    }
    while sent < FRAME_CAP {
        let Ok((back, batch)) = tokio::task::spawn_blocking(move || {
            let batch = reader.read_batch(READ_BATCH);
            (reader, batch)
        })
        .await
        else {
            return;
        };
        reader = back;
        let Ok(batch) = batch else {
            tracing::warn!(event = "rpp.ledger.read_failed", "closing ledger stream");
            return;
        };
        if batch.is_empty() {
            tokio::select! {
                () = tokio::time::sleep(POLL_INTERVAL) => {}
                () = tx.closed() => return,
            }
            continue;
        }
        for item in batch {
            let frame = match item {
                LedgerRead::Reset { reason, cursor } => Event::default()
                    .id(cursor.to_string())
                    .event("ledger.reset")
                    .data(
                        json!({
                            "reason": reason.as_str(),
                            "epoch": cursor.epoch,
                            "cursor": cursor.to_string(),
                        })
                        .to_string(),
                    ),
                LedgerRead::Line { raw, cursor } => {
                    let Some((name, data)) = project_line(&raw) else {
                        continue;
                    };
                    Event::default()
                        .id(cursor.to_string())
                        .event(name)
                        .data(data.to_string())
                }
            };
            if tx.send(frame).await.is_err() {
                return;
            }
            sent += 1;
            if sent >= FRAME_CAP {
                break;
            }
        }
    }
}

/// Admission [`Spark`] for the route; the verb is the events subscription.
fn build_spark(state: &Arc<AppState>, jwt: &ValidatedJwt) -> Result<Spark, ApiError> {
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
    http_request_to_spark(&rig, jwt, Verb::SubscribeEvents, None)
        .map_err(|e| state.reject_with_request_id(e, new_request_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequenced_line_is_projected_to_its_documented_fields() {
        let raw = r#"{"seq":7,"mol_seq":2,"timestamp":"2026-10-06T10:00:00Z","emitter_id":"w","type":"molecule_step_completed","molecule_id":"m-1","step":1,"total":3,"evidence":"done","worktree_path":"/x"}"#;
        let (name, frame) = project_line(raw).unwrap();
        assert_eq!(name, "molecule_step_completed");
        assert_eq!(
            frame,
            json!({
                "schema_version": 1, "type": "molecule_step_completed",
                "molecule_id": "m-1", "timestamp": "2026-10-06T10:00:00Z",
                "seq": 7, "mol_seq": 2, "step": 1, "total": 3, "evidence": "done"
            })
        );
    }

    #[test]
    fn an_unsequenced_file_store_line_is_served_with_schema_version_one() {
        let raw = r#"{"timestamp":"2026-07-17T15:49:06Z","hash":"abc","kind":"molecule_evolved","molecule_id":"m-1","step":0,"total":2}"#;
        let (name, frame) = project_line(raw).unwrap();
        assert_eq!(name, "molecule_evolved");
        assert_eq!(frame["schema_version"], 1);
        assert!(frame.get("seq").is_none());
        assert!(frame.get("hash").is_none());
    }

    #[test]
    fn the_kind_tag_of_a_legacy_line_is_not_read_as_a_collapse_kind() {
        let legacy = r#"{"kind":"molecule_collapsed","molecule_id":"m-1","reason":"r"}"#;
        let (_, frame) = project_line(legacy).unwrap();
        assert!(frame.get("kind").is_none());
        let current = r#"{"type":"molecule_collapsed","molecule_id":"m-1","reason":"r","kind":"worker_crashed"}"#;
        let (_, frame) = project_line(current).unwrap();
        assert_eq!(frame["kind"], "worker_crashed");
    }

    #[test]
    fn molecule_spelled_molecule_or_mol_id_is_served_as_molecule_id() {
        let a = r#"{"type":"merge_completed","molecule":"m-1","branch":"feat/m-1","result":"ok"}"#;
        assert_eq!(project_line(a).unwrap().1["molecule_id"], "m-1");
        let b = r#"{"type":"model_selected","mol_id":"m-2","model":"x"}"#;
        assert_eq!(project_line(b).unwrap().1["molecule_id"], "m-2");
    }

    #[test]
    fn operator_lines_and_lines_without_a_molecule_are_not_served() {
        for raw in [
            r#"{"type":"operator_present","molecule_id":"m-1"}"#,
            r#"{"type":"operator_signed","molecule_id":"m-1"}"#,
            r#"{"type":"worker_killed","worker_id":"w"}"#,
            r#"{"type":"molecule_completed"}"#,
            r#"{"type":"some_future_event","molecule_id":"m-1","secret":"x"}"#,
            "not json",
            "[]",
        ] {
            assert!(project_line(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn a_session_id_is_digested_and_never_served() {
        let raw = r#"{"type":"session_presence","session_id":"5874d35a-0c3c","molecule_id":"m-1","state":"idle","role":"worker"}"#;
        let (_, frame) = project_line(raw).unwrap();
        let text = frame.to_string();
        assert!(!text.contains("5874d35a"), "{text}");
        assert_eq!(frame["session_digest"].as_str().unwrap().len(), 16);
        assert_eq!(frame["state"], "idle");
    }

    #[test]
    fn a_line_own_schema_version_is_kept() {
        let raw = r#"{"type":"harvested","schema_version":2,"molecule_id":"m-1","success":true}"#;
        assert_eq!(project_line(raw).unwrap().1["schema_version"], 2);
    }
}
