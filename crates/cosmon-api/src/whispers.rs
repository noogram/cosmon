// SPDX-License-Identifier: AGPL-3.0-only

//! `/whispers`, `/whispers/{id}/archive`, `/whispers/{id}/spark`,
//! `/whisper/{mol_id}` endpoints.
//!
//! The whisper inbox is the filesystem drop zone populated by
//! `cosmon-matrix-tick` and friends (ADR-064): one markdown file per
//! incoming signal, nested under a room directory. This module exposes
//! a thin HTTP view over that directory tree so the iOS pilot can
//! display unprocessed whispers and act on them (archive / spark) via
//! Tailscale.
//!
//! The singular `/whisper/{mol_id}` route is the *outbound* counterpart:
//! it injects a perturbation payload into a live worker's tmux pane by
//! shelling out to `cs whisper <mol_id> -m <body>`. It is used by the
//! iOS pilot to whisper into a running molecule from the molecule's
//! detail view.
//!
//! See [ADR-038](../../../docs/adr/038-whisper-perturbation-port.md) —
//! `/whispers/{id}/spark` is a UI-facing promotion, not an in-loop
//! worker perturbation. It shells out to `cs spark` exactly like the
//! operator would from a terminal. `/whisper/{mol_id}` is the inverse:
//! the existing whisper substrate enforces §8j ingress binding (size
//! limit, rate limit, allowed-command check) so the route does not
//! bypass any binding rule.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::instrumentation::{record_in_process, InvocationMode};
use crate::{cs_exec_error, parse_cs_json, run_cs, ApiError, AppState};

/// Default number of whispers returned by `GET /whispers`.
const DEFAULT_LIMIT: usize = 50;
/// Upper bound; protects the iOS client from an accidental unbounded scan.
const MAX_LIMIT: usize = 500;

/// `GET /whispers` query string.
#[derive(Debug, Deserialize)]
pub(crate) struct WhispersQuery {
    /// Maximum number of whispers to return. Clamped to `[1, 500]`.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One parsed whisper, matching the schema the iOS pilot expects.
#[derive(Debug, Serialize)]
pub(crate) struct Whisper {
    /// Filename stem (the `<origin_ts>-<event_id>` id used in the matrix-tick layout).
    pub id: String,
    /// Originating Matrix room (e.g. `!room:matrix.org`).
    pub room_id: String,
    /// Normalised nucleon identity (ADR-061). Preserved verbatim so the
    /// caller can pass it back through `POST /whispers/{id}/spark`.
    pub sender_nucleon_id: Option<String>,
    /// Matrix user id (e.g. `@you:matrix.org`).
    pub sender_mxid: Option<String>,
    /// ISO-8601 UTC timestamp emitted by `cosmon-matrix-tick`.
    pub received_at: String,
    /// Raw message body — everything after the closing `---` of the frontmatter.
    pub body: String,
    /// Absolute filesystem path (kept for debugging / manual inspection).
    pub path: String,
}

/// `GET /whispers?limit=50` — list the most recent whispers in the inbox.
pub(crate) async fn list_whispers(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WhispersQuery>,
) -> Result<Json<Value>, ApiError> {
    record_in_process(
        &state,
        "/whispers",
        "<scan-whispers-inbox>",
        InvocationMode::InProcessStateRead,
        || {
            let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
            let root = state.resolve_whispers_inbox_root();
            let mut whispers = scan_whispers(&root).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scan whispers inbox: {e}"),
                )
            })?;
            // Newest first — `received_at` is ISO-8601 UTC so lexical sort is chronological.
            whispers.sort_by(|a, b| b.received_at.cmp(&a.received_at));
            whispers.truncate(limit);
            Ok(Json(serde_json::json!({ "whispers": whispers })))
        },
    )
}

/// `POST /whispers/{id}/archive` — move the whisper markdown file from
/// `<inbox_root>/<room>/<id>.md` to `<archived_root>/<room>/<id>.md`.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateWrite`]. The
/// handler does not modify cosmon `state.json`, but it does mutate
/// disk state (the whisper inbox layout) — that's the closest match.
pub(crate) async fn archive_whisper(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    record_in_process(
        &state,
        "/whispers/{id}/archive",
        "<archive-whisper>",
        InvocationMode::InProcessStateWrite,
        || {
            let inbox = state.resolve_whispers_inbox_root();
            let archived = state.resolve_whispers_archived_root();
            let source = find_whisper(&inbox, &id).ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    format!("whisper '{id}' not found under {}", inbox.display()),
                )
            })?;
            let room_dir = source
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "cannot determine room directory from source path",
                    )
                })?;
            let dest_dir = archived.join(room_dir);
            std::fs::create_dir_all(&dest_dir).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("create archived dir {}: {e}", dest_dir.display()),
                )
            })?;
            let dest = dest_dir.join(source.file_name().unwrap_or_default());
            std::fs::rename(&source, &dest).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("rename {} -> {}: {e}", source.display(), dest.display()),
                )
            })?;
            Ok(Json(serde_json::json!({
                "ok": true,
                "id": id,
                "archived_path": dest.to_string_lossy(),
            })))
        },
    )
}

/// `POST /whispers/{id}/spark` body. Both fields are optional: if
/// omitted the handler defaults to the whisper's body text and sender
/// nucleon, exactly the manual equivalent of
/// `cs spark --nucleon <sender_nucleon_id> "<body>"`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SparkRequest {
    /// Optional override for the spark text. Defaults to the whisper body.
    pub text: Option<String>,
    /// Optional override for the sparker identity. Defaults to the whisper's
    /// `sender_nucleon_id`. When both this field and the whisper are silent,
    /// `cs spark` falls back to `git user.email`.
    pub nucleon: Option<String>,
}

/// `POST /whispers/{id}/spark` — shell out to `cs spark` to capture the
/// whisper body as an `idea` molecule (ADR-061). Idempotency is not
/// guaranteed — the operator may deliberately spark the same whisper
/// multiple times (each spark is a separate molecule).
pub(crate) async fn spark_whisper(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<SparkRequest>>,
) -> Result<Json<Value>, ApiError> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let inbox = state.resolve_whispers_inbox_root();
    let path = find_whisper(&inbox, &id).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            format!("whisper '{id}' not found under {}", inbox.display()),
        )
    })?;
    let content = std::fs::read_to_string(&path).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read {}: {e}", path.display()),
        )
    })?;
    let parsed = parse_whisper(&path, &content).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse whisper frontmatter at {}", path.display()),
        )
    })?;
    let text = req.text.unwrap_or(parsed.body);
    if text.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "whisper body is empty — refusing to spark an empty molecule",
        ));
    }
    let nucleon = req.nucleon.or(parsed.sender_nucleon_id);
    let mut args: Vec<String> = vec!["--json".into(), "spark".into()];
    if let Some(n) = nucleon.as_ref().filter(|n| !n.trim().is_empty()) {
        args.push("--nucleon".into());
        args.push(n.clone());
    }
    args.push(text);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_cs(&state, "/whispers/{id}/spark", &args_ref).await?;
    if !output.status.success() {
        return Err(cs_exec_error(&output));
    }
    let value = parse_cs_json(&output.stdout)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "whisper_id": id,
        "spark": value,
    })))
}

/// `POST /whisper/{mol_id}` body — the outbound perturbation payload.
///
/// `body` is the inline text injected into the worker's tmux pane.
/// `binding` is the §8j ingress binding id for audit; it is accepted
/// and stored in the response metadata but the substrate-level binding
/// enforcement lives inside `cs whisper` itself (allowed-command check,
/// rate limit, size limit).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct SendWhisperRequest {
    /// Inline payload. Empty / whitespace-only bodies are rejected
    /// before we shell out so we don't spend a tmux round-trip on a
    /// no-op.
    pub body: String,
    /// Optional §8j ingress binding identifier — recorded for audit in
    /// a future version; currently advisory.
    #[serde(default)]
    pub binding: Option<String>,
}

/// `POST /whisper/{mol_id}` — shell out to `cs whisper <mol_id> -m
/// <body>` to inject a perturbation into a live worker's pane.
///
/// Returns `204 No Content` on success. On any failure from the
/// underlying CLI (missing molecule, rate-limited, size-limited, pane
/// not running an allowed command) returns `400` with the stderr text
/// copied into the JSON `error` field. v0 deliberately does not expose
/// the structured exit codes — the Matrix/iOS pilots treat whisper as
/// advisory and a single 400 is sufficient UX.
#[allow(dead_code)]
pub(crate) async fn send_whisper(
    State(state): State<Arc<AppState>>,
    AxumPath(mol_id): AxumPath<String>,
    Json(req): Json<SendWhisperRequest>,
) -> Result<StatusCode, ApiError> {
    let text = req.body.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "whisper body is empty",
        ));
    }
    // Defence in depth: the molecule id is passed as an argv entry so
    // there is no shell to inject into, but an id containing `/` or
    // `..` would still be rejected by `MoleculeId::new` at a deeper
    // layer — shortcut here so the error is returned synchronously.
    if mol_id.is_empty() || mol_id.contains('/') || mol_id.contains('\\') || mol_id.contains("..") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid molecule id '{mol_id}'"),
        ));
    }
    let _ = &req.binding; // accepted for forward-compat; not forwarded to CLI today.
    let output = run_cs(
        &state,
        "/whisper/{mol_id}",
        &["whisper", &mol_id, "-m", text],
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let message = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!(
                "cs whisper exited with status {}",
                output.status.code().unwrap_or(-1)
            )
        };
        return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Walk the inbox root (one level of room directories, one level of
/// `.md` files) and parse every whisper we find. Missing root is
/// treated as "no whispers yet" rather than an error so `GET /whispers`
/// works on a brand-new project that has never received one.
fn scan_whispers(root: &Path) -> std::io::Result<Vec<Whisper>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for room_entry in std::fs::read_dir(root)? {
        let room_entry = room_entry?;
        if !room_entry.file_type()?.is_dir() {
            continue;
        }
        for file_entry in std::fs::read_dir(room_entry.path())? {
            let file_entry = file_entry?;
            let path = file_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(w) = parse_whisper(&path, &content) {
                out.push(w);
            }
        }
    }
    Ok(out)
}

/// Locate the `<id>.md` file under any room subdirectory. Returns
/// `None` for both "file does not exist" and "traversal attempt" —
/// callers render a single 404 either way.
fn find_whisper(root: &Path, id: &str) -> Option<PathBuf> {
    // Traversal defence: the id is a URL path segment and must not
    // contain separators or `..`. A legitimate whisper id is
    // `<origin_ts>-<event_id>` with only `[a-zA-Z0-9_-]`.
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    if !root.exists() {
        return None;
    }
    let target = format!("{id}.md");
    let entries = std::fs::read_dir(root).ok()?;
    for room_entry in entries.flatten() {
        if !room_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let candidate = room_entry.path().join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parse a single whisper file. Returns `None` when the frontmatter is
/// malformed; callers skip those rather than error, so a half-written
/// file from a concurrent matrix-tick does not poison the list.
fn parse_whisper(path: &Path, content: &str) -> Option<Whisper> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = &rest[..end];
    let body = rest[end + 5..].trim_matches('\n').to_owned();

    let mut room_id = String::new();
    let mut sender_mxid = None;
    let mut sender_nucleon_id = None;
    let mut received_at = String::new();

    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let value = v.trim().trim_matches('"').to_owned();
        if value.is_empty() {
            continue;
        }
        match key {
            "room_id" => room_id = value,
            "sender_mxid" => sender_mxid = Some(value),
            "sender_nucleon_id" => sender_nucleon_id = Some(value),
            "received_at" => received_at = value,
            _ => {}
        }
    }

    let id = path.file_stem()?.to_str()?.to_owned();
    Some(Whisper {
        id,
        room_id,
        sender_nucleon_id,
        sender_mxid,
        received_at,
        body,
        path: path.to_string_lossy().into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_whisper(root: &Path, room: &str, id: &str, received_at: &str, body: &str) -> PathBuf {
        let dir = root.join(room);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        let content = format!(
            "---\n\
             event_id: \"${id}\"\n\
             sender_mxid: \"@tenant_auditor:matrix.org\"\n\
             sender_nucleon_id: \"tenant_auditor\"\n\
             room_id: \"!{room}\"\n\
             received_at: \"{received_at}\"\n\
             ---\n\n\
             {body}\n"
        );
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_whisper_reads_frontmatter_and_body() {
        let tmp = TempDir::new().unwrap();
        let path = write_whisper(
            tmp.path(),
            "_room_matrix.org",
            "123-abc",
            "2026-04-22T21:32:37Z",
            "Salut 👋",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let w = parse_whisper(&path, &content).unwrap();
        assert_eq!(w.id, "123-abc");
        assert_eq!(w.sender_nucleon_id.as_deref(), Some("tenant_auditor"));
        assert_eq!(w.sender_mxid.as_deref(), Some("@tenant_auditor:matrix.org"));
        assert_eq!(w.received_at, "2026-04-22T21:32:37Z");
        assert_eq!(w.body, "Salut 👋");
        assert_eq!(w.room_id, "!_room_matrix.org");
    }

    #[test]
    fn scan_whispers_returns_all_rooms_sorted_by_caller() {
        let tmp = TempDir::new().unwrap();
        write_whisper(tmp.path(), "room-a", "1-a", "2026-04-22T10:00:00Z", "a");
        write_whisper(tmp.path(), "room-b", "2-b", "2026-04-22T11:00:00Z", "b");
        let whispers = scan_whispers(tmp.path()).unwrap();
        assert_eq!(whispers.len(), 2);
        let ids: Vec<&str> = whispers.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&"1-a"));
        assert!(ids.contains(&"2-b"));
    }

    #[test]
    fn scan_whispers_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert_eq!(scan_whispers(&missing).unwrap().len(), 0);
    }

    #[test]
    fn find_whisper_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        write_whisper(tmp.path(), "room-a", "plain", "2026-04-22T10:00:00Z", "x");
        assert!(find_whisper(tmp.path(), "plain").is_some());
        assert!(find_whisper(tmp.path(), "../plain").is_none());
        assert!(find_whisper(tmp.path(), "room-a/plain").is_none());
        assert!(find_whisper(tmp.path(), "").is_none());
    }
}
