// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/status` — the poll surface.
//!
//! # No blocked thread server-side
//!
//! This route is the whole server-side of `cosmon-remote wait`. There is
//! deliberately no `wait` route: a route that blocks until a molecule
//! moves would hold one server thread (and one connection, and one
//! timeout to negotiate with every proxy on the path) per waiting client,
//! and the state it would hold — *who is waiting on what* — is state the
//! adapter has no reason to own. The client owns its own patience. What
//! the server owes it is an answer cheap enough to ask for repeatedly,
//! which is this route and nothing more.
//!
//! # Why a route rather than a projection of the molecule read
//!
//! `GET /v1/molecules/{id}` answers the same question. It also, on the
//! way, folds a coupling report, the per-molecule token totals and the
//! model attribution — three scans of append-only logs that grow with the
//! molecule's lifetime. A poller would therefore pay *more* per poll the
//! longer it waited, which is backwards. Three reasons the fix is a
//! second route and not a `?fields=status` selection on the first:
//!
//! * **Cost.** A `fields=` filter applied *after* `observe` saves bytes
//!   and no reads; applied *before* it, it is this handler with a query
//!   parameter in front — the same two code paths, one of them hidden.
//! * **Cache.** Two representations behind one URL need two entity-tags
//!   keyed by a query parameter, which is the `Vary` hazard intermediary
//!   caches get wrong. A distinct resource has one tag.
//! * **Canon.** The §8p canon keys on `METHOD PATH`. A query parameter
//!   grows the surface without appearing in the log that exists to record
//!   surface growth — the drift channel the canon closes.
//!
//! # Conditional polling
//!
//! The body carries a per-request id, so two answers that *mean* the same
//! thing are not the same bytes. The entity-tag is therefore **weak**
//! (`W/"…"`), derived from [`StatusView::validator`] — the `(status,
//! updated_at)` pair every other field is a function of. A poller that
//! echoes it in `If-None-Match` gets `304` and no body while the molecule
//! has not moved.
//!
//! `Cache-Control: no-cache` rides along: an intermediary may store the
//! answer, and must revalidate before serving it. A molecule's status is
//! exactly the fact nobody may serve stale.
//!
//! # Pipeline
//!
//! The five clauses of every molecule read — bearer, JWT, scope,
//! admission, tenant-isolated load — then one projection. Same
//! `cosmon:molecule:read` as the full read: this is a strictly smaller
//! answer about the same object, and a scope of its own would split one
//! grant in two and let the halves drift.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use cosmon_core::id::MoleculeId;
use cosmon_state::ops::{StatusJson, StatusView};
use serde_json::json;

use crate::admission::Verb;
use crate::auth::scopes::{MOLECULE_READ, MOLECULE_WRITE};
use crate::error::ApiError;
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, extract_bearer, status_public,
};
use crate::AppState;

/// Render a [`StatusView`]'s validator as a weak HTTP entity-tag.
///
/// Weak because the representation is not byte-stable for a stable
/// answer: the envelope carries a fresh `request_id` each time. `W/`
/// states exactly that — semantically equivalent, not identical — which
/// is the honest tag and the one `If-None-Match` compares with.
///
/// The validator is quoted and its `"` and `\` escaped, so a status word
/// or a timestamp can never terminate the tag early. Neither can contain
/// those bytes today; the escape is here so that a future one cannot
/// forge a header.
#[must_use]
pub fn etag_for(view: &StatusView) -> String {
    let escaped = view.validator().replace('\\', "\\\\").replace('"', "\\\"");
    format!("W/\"{escaped}\"")
}

/// Whether an `If-None-Match` header matches the current tag.
///
/// RFC 9110 §13.1.2: the header is a comma-separated list, `*` matches
/// any existing representation, and the comparison for `If-None-Match` is
/// the **weak** one — `W/"x"` and `"x"` match. Both forms are accepted so
/// a client that strips the `W/` prefix still gets its `304`.
#[must_use]
pub fn if_none_match_hits(header_value: &str, current: &str) -> bool {
    let strip = |t: &str| t.trim().trim_start_matches("W/").trim().to_owned();
    let current = strip(current);
    header_value
        .split(',')
        .any(|candidate| candidate.trim() == "*" || strip(candidate) == current)
}

/// `GET /v1/molecules/{id}/status` — see module docs.
pub async fn get_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Response, ApiError> {
    // 1 & 2 — bearer + JWT.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 3 — molecule:read scope (write implies read).
    authorise_scope_public(
        &state,
        &jwt,
        "status",
        &[MOLECULE_READ, MOLECULE_WRITE],
        MOLECULE_READ,
    )?;

    // 4 — admission. This is a molecule read; reuse the verb rather than
    //     mint a second one for a smaller answer about the same object.
    let spark = build_spark_public(&state, &jwt, Verb::ObserveMolecule, Some(&molecule_id_str))?;

    // 5 — a malformed id is a 404, never a 400 (turing §8.2.3 — never emit
    //     an existence oracle on the wire).
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    let view = status_public(&state, &spark, &molecule_id)?;
    let etag = etag_for(&view);

    // The conditional answer. Computed AFTER the read, never instead of
    // it: the tag is a fact about the current state, so there is no
    // cached verdict to serve without looking. What `304` saves is the
    // body and its serialisation, which is what a poller re-reads.
    let conditional = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| if_none_match_hits(v, &etag));

    let mut response = if conditional {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let body = StatusJson::from_view(&view);
        axum::response::Json(json!({
            "request_id": spark.request_id,
            "molecule_id": molecule_id_str,
            "status": body.status,
            "phase": body.phase,
            "updated_at": body.updated_at,
            "terminal": body.terminal,
        }))
        .into_response()
    };

    let headers_out = response.headers_mut();
    if let Ok(value) = etag.parse() {
        headers_out.insert(header::ETAG, value);
    }
    headers_out.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    headers_out.insert(
        header::LAST_MODIFIED,
        header::HeaderValue::from_str(&view.updated_at.to_rfc2822())
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(status: &str, updated: &str) -> StatusView {
        let raw = serde_json::json!({
            "id": "task-20260907-b25f",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": status,
            "created_at": "2026-09-07T10:00:00Z",
            "updated_at": updated,
            "total_steps": 2,
            "current_step": 1,
            "variables": {},
            "completed_steps": [],
            "links": [],
        });
        let data: cosmon_state::MoleculeData =
            serde_json::from_value(raw).expect("the minimal persisted form deserialises");
        StatusView::from_data(&data)
    }

    #[test]
    fn the_tag_is_weak_and_moves_with_the_molecule() {
        let a = etag_for(&view("running", "2026-09-07T12:00:00Z"));
        assert!(a.starts_with("W/\""), "{a} must be a weak tag");
        assert_eq!(a, etag_for(&view("running", "2026-09-07T12:00:00Z")));
        assert_ne!(a, etag_for(&view("completed", "2026-09-07T12:00:00Z")));
        assert_ne!(a, etag_for(&view("running", "2026-09-07T12:00:01Z")));
    }

    #[test]
    fn if_none_match_accepts_both_spellings_a_star_and_a_list() {
        let tag = etag_for(&view("running", "2026-09-07T12:00:00Z"));
        let bare = tag.trim_start_matches("W/").to_owned();
        assert!(if_none_match_hits(&tag, &tag));
        assert!(if_none_match_hits(&bare, &tag), "a stripped W/ still hits");
        assert!(if_none_match_hits("*", &tag));
        assert!(if_none_match_hits(&format!("\"other\", {tag}"), &tag));
        assert!(!if_none_match_hits("\"other\"", &tag));
    }
}
