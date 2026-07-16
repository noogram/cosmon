// SPDX-License-Identifier: AGPL-3.0-only

//! `/molecules/{id}` and the two non-destructive write verbs
//! (`/molecules/{id}/tackle`, `/molecules/{id}/tag`).
//!
//! - `GET /molecules/{id}` — read-only observe. **Library-first**: the
//!   handler reads `<state>/fleets/*/molecules/<id>/state.json` directly
//!   via [`cosmon_state::ops::observe`], no shell-out. This was the
//!   first verb promoted from a subprocess call to an in-process
//!   library call.
//! - `POST /molecules/{id}/tag` — first **mutant** library-first verb.
//!   Calls [`cosmon_state::ops::tag`] in-process via
//!   [`InvocationMode::InProcessStateWrite`].
//!   Same byte-stable JSON wire format as the cs-cli.
//! - `POST /molecules/{id}/tackle` — still shell-outs to `cs` (CLI-first
//!   for writers requiring tmux + worktree side-effects, ADR-080).

use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Json;
use cosmon_core::auth::Subject;
use cosmon_core::id::MoleculeId;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::ops::{observe, tag as ops_tag, ObserveError, ObserveJson, TagError, TagJson};
use serde::Deserialize;
use serde_json::Value;

use crate::instrumentation::{record_in_process, InvocationMode};
use crate::{cs_exec_error, parse_cs_json, run_cs, ApiError, AppState};

/// Defence-in-depth for `id` path segments — matches the cs-cli
/// convention (lowercase prefix, dash, date, dash, suffix) and nothing
/// that could hop out of the arg list.
fn validate_molecule_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "empty molecule id"));
    }
    if id.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "molecule id too long",
        ));
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':');
    if !ok {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "molecule id contains illegal characters",
        ));
    }
    Ok(())
}

/// Same shape as [`validate_molecule_id`], but permits the `=`, `.` and
/// `/` characters that legitimately appear in tag values (e.g.
/// `temp:hot`, `owner:user@host`, `path:foo/bar`).
fn validate_tag(tag: &str) -> Result<(), ApiError> {
    if tag.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "empty tag"));
    }
    if tag.len() > 128 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "tag too long"));
    }
    // Refuse anything that could be misread as a flag or shell metachar.
    // We accept the printable-ASCII set minus the dangerous subset.
    let banned = [
        ' ', '\t', '\n', '\r', '\\', '"', '\'', '`', '$', '|', '&', ';', '<', '>', '(', ')', '{',
        '}', '[', ']', '*', '?', '!', '#',
    ];
    if tag.starts_with('-') {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "tag must not start with '-'",
        ));
    }
    if tag.chars().any(|c| banned.contains(&c) || c.is_control()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "tag contains forbidden characters",
        ));
    }
    Ok(())
}

/// `GET /molecules/{id}` — read-only molecule observation, in-process.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateRead`]. The
/// handler calls [`observe`] which loads `state.json` from disk — no
/// `cs` subprocess, no fork-exec. This is the first verb promoted from
/// a subprocess shell-out to an in-process library call, which removes
/// the fork-exec latency from the read path.
///
/// The JSON body is the same `ObserveJson` shape that
/// `cs observe <id> --json` prints, so external scripts that already
/// consume the CLI output can switch to the HTTP route without
/// reformatting. `molecule_dir` is intentionally returned empty — the
/// iOS pilot does not consume the field today, and exposing the
/// daemon's filesystem layout is unnecessary surface area.
pub(crate) async fn get_molecule(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ObserveJson>, ApiError> {
    validate_molecule_id(&id)?;
    let mol_id = MoleculeId::new(&id).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid molecule id \"{id}\": {e}"),
        )
    })?;

    let result = record_in_process(
        &state,
        "/molecules/{id}",
        "observe",
        InvocationMode::InProcessStateRead,
        || -> Result<Json<ObserveJson>, ApiError> {
            let state_dir = state.resolve_cosmon_state_dir();
            let store = FileStore::new(&state_dir);
            // T-RECTIFY (task-20260503-09c8) — V0 mono-tenant loopback.
            // cs-api today runs as the operator backend (no JWT auth at
            // the boundary yet); T-RPP-V0 will replace `Subject::operator()`
            // with `Subject::from_jwt_claims(...)` once the adapter
            // verifies the bearer token.
            let view =
                observe(&store, &state_dir, &Subject::operator(), &mol_id).map_err(
                    |e| match e {
                        ObserveError::MoleculeNotFound(_) => ApiError::new(
                            StatusCode::NOT_FOUND,
                            format!("molecule not found: {id}"),
                        ),
                        ObserveError::StoreUnavailable(msg) => ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("state store error: {msg}"),
                        ),
                        // T-V1-API-SHAPE — `ObserveError` is `#[non_exhaustive]`
                        // so V1 can add a `ByokRequired` variant in a minor
                        // bump. Until then any future variant collapses to a
                        // generic 500 with a stable message; the wildcard
                        // arm is the structural unblock, not a silent drop.
                        _ => ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "unexpected observe error".to_string(),
                        ),
                    },
                )?;
            Ok(Json(ObserveJson::from_view(&view, "")))
        },
    );
    result
}

/// `POST /molecules/{id}/tackle` — shell out to `cs tackle <id>`.
///
/// This is the iOS-side equivalent of tapping "Tackle" in the mac-pilot
/// Inbox. The worker is spawned on the Mac (the machine running
/// cs-api); the iPhone just asks for it. This follows the design split
/// where *iOS is a presence sensor and the Mac is the cockpit*:
/// destructive verbs (`done`, `collapse`) stay Mac-only and are **not**
/// exposed here.
pub(crate) async fn tackle_molecule(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    validate_molecule_id(&id)?;
    let output = run_cs(&state, "/molecules/{id}/tackle", &["--json", "tackle", &id]).await?;
    if !output.status.success() {
        return Err(cs_exec_error(&output));
    }
    // `cs tackle --json` may emit a JSON envelope; if parsing fails we
    // still return a success with an empty body so the pilot can
    // refresh the inbox.
    let value = parse_cs_json(&output.stdout).unwrap_or(Value::Null);
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": id,
        "tackle": value,
    })))
}

/// `POST /molecules/{id}/tag` body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TagRequest {
    /// Tags to add (repeatable on the CLI as `--add <tag>`).
    pub add: Vec<String>,
    /// Tags to remove (repeatable on the CLI as `--remove <tag>`).
    pub remove: Vec<String>,
}

/// `POST /molecules/{id}/tag` — in-process retag, library-first.
///
/// The operator can promote a molecule from `temp:warm` to `temp:hot`
/// (or the reverse) directly from the iPad without opening a Mac
/// terminal — this is the primary write verb exposed to iOS, where the
/// device acts as a presence sensor rather than a full cockpit.
///
/// **Invocation mode:** [`InvocationMode::InProcessStateWrite`]. The
/// handler calls [`cosmon_state::ops::tag`] which loads `state.json`,
/// mutates the `tags` field, and persists the document atomically (via
/// the canonical tempfile + rename in `cosmon-filestore`). No `cs`
/// subprocess, no fork-exec — the first **mutant** (state-writing) verb
/// promoted from a subprocess shell-out to an in-process library call.
pub(crate) async fn tag_molecule(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<TagRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_molecule_id(&id)?;
    let mol_id = MoleculeId::new(&id).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid molecule id \"{id}\": {e}"),
        )
    })?;

    if req.add.is_empty() && req.remove.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "tag request must include at least one `add` or `remove`",
        ));
    }
    for tag in req.add.iter().chain(req.remove.iter()) {
        validate_tag(tag)?;
    }

    // Parse into typed `Tag` values **before** entering the recorded
    // closure — a `Tag::new` failure is a 400 surfaced through `ApiError`,
    // not an event we want logged as an in-process write.
    let add_tags: Vec<Tag> = req
        .add
        .iter()
        .map(|s| {
            Tag::new(s.clone()).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid add tag `{s}`: {e}"),
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let remove_tags: Vec<Tag> = req
        .remove
        .iter()
        .map(|s| {
            Tag::new(s.clone()).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid remove tag `{s}`: {e}"),
                )
            })
        })
        .collect::<Result<_, _>>()?;

    let added_echo = req.add.clone();
    let removed_echo = req.remove.clone();

    record_in_process(
        &state,
        "/molecules/{id}/tag",
        "tag",
        InvocationMode::InProcessStateWrite,
        || -> Result<Json<Value>, ApiError> {
            let state_dir = state.resolve_cosmon_state_dir();
            let store = FileStore::new(&state_dir);
            // T-AUTHZ-INSTR — V0 placeholder. cs-api today runs as the
            // operator backend (no JWT auth at the boundary yet);
            // T-RPP-V0 will replace this with the JWT-derived
            // `jwt:<sub>` once the adapter wires real subjects.
            let delta = ops_tag(
                &store,
                &state_dir,
                "operator",
                &mol_id,
                &add_tags,
                &remove_tags,
            )
            .map_err(|e| match e {
                TagError::MoleculeNotFound(_) => {
                    ApiError::new(StatusCode::NOT_FOUND, format!("molecule not found: {id}"))
                }
                TagError::EmptyRequest => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "tag request must include at least one `add` or `remove`",
                ),
                TagError::ProtectedReservation(tag) => ApiError::new(
                    StatusCode::FORBIDDEN,
                    format!("protected runtime reservation cannot be removed: {tag}"),
                ),
                TagError::ProtectedDecisionOptIn(tag) => ApiError::new(
                    StatusCode::FORBIDDEN,
                    format!("protected runtime decision opt-in cannot be added: {tag}"),
                ),
                TagError::StoreUnavailable(msg) => ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("state store error: {msg}"),
                ),
            })?;
            // Match the legacy wire envelope so external scripts keep
            // working: `{ok, id, added, removed, tag: {...TagJson...}}`.
            let inner = TagJson::from_delta(&delta);
            Ok(Json(serde_json::json!({
                "ok": true,
                "id": id,
                "added": added_echo,
                "removed": removed_echo,
                "tag": inner,
            })))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_molecule_id_accepts_normal_ids() {
        assert!(validate_molecule_id("task-20260423-f74e").is_ok());
        assert!(validate_molecule_id("delib-20260422-f6d6").is_ok());
        assert!(validate_molecule_id("idea-42").is_ok());
    }

    #[test]
    fn validate_molecule_id_rejects_shell_metacharacters() {
        assert!(validate_molecule_id("task-$(rm -rf /)").is_err());
        assert!(validate_molecule_id("task;ls").is_err());
        assert!(validate_molecule_id("../etc/passwd").is_err());
        assert!(validate_molecule_id("").is_err());
    }

    #[test]
    fn validate_tag_accepts_typed_labels() {
        assert!(validate_tag("temp:hot").is_ok());
        assert!(validate_tag("temp:warm").is_ok());
        assert!(validate_tag("owner:you").is_ok());
        assert!(validate_tag("ux").is_ok());
    }

    #[test]
    fn validate_tag_rejects_leading_dash_and_metachars() {
        assert!(validate_tag("").is_err());
        assert!(validate_tag("--add").is_err());
        assert!(validate_tag("temp hot").is_err());
        assert!(validate_tag("temp;rm").is_err());
        assert!(validate_tag("temp|cat").is_err());
    }
}
