// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/artifacts`,
//! `GET /v1/molecules/{id}/artifacts/{token}`,
//! `PUT /v1/molecules/{id}/artifacts/{name}` — artifact endpoints
//! (e653 spec).
//!
//! Artifacts are the worker's outputs on disk under
//! `/tmp/cosmon/<noyau>/<molecule_id>/`. The convention is set at
//! `tackle` time (the adapter creates the directory and exports
//! `COSMON_ARTIFACT_DIR` into the worker subprocess env, see
//! [`crate::subprocess`]). The worker writes files freely into that
//! directory; the three routes here expose it to the pilot.
//!
//! # Pipeline (mirrors the molecule routes' six-step shape)
//!
//! 1. Extract bearer.
//! 2. Validate JWT.
//! 3. Scope check — `cosmon:artifact:read` for the two GETs,
//!    `cosmon:artifact:write` for the PUT. `:read` is *not* implied by
//!    `:molecule:read` (artifacts can carry payloads whose exposure
//!    is decided independently from molecule observability).
//! 4. Admission boundary (`http_request_to_spark`).
//! 5. Filesystem operation against `<artifact_root>/<noyau>/<molecule_id>/`.
//! 6. Return envelope / stream bytes.
//!
//! # Tokens
//!
//! The manifest assigns each entry a stable, opaque token of the form
//! `art_<24 base32 chars>` deterministically derived from the
//! filename. The token isolates the URL surface from the on-disk
//! filename — useful when a tenant uploads filenames that would not
//! round-trip cleanly through a URL path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use crate::audit::new_request_id;
use crate::auth::scopes::{ARTIFACT_READ, ARTIFACT_WRITE};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::{JwtVerifier, ValidatedJwt};
use crate::AppState;

/// Root directory under which per-molecule artifact dirs live. The
/// convention is the same on host and inside the container; the
/// adapter creates `<root>/<noyau>/<molecule_id>/` at tackle time.
pub const DEFAULT_ARTIFACT_ROOT: &str = "/tmp/cosmon";

/// Env var name set on the worker subprocess pointing at its
/// per-molecule artifact directory.
pub const ENV_COSMON_ARTIFACT_DIR: &str = "COSMON_ARTIFACT_DIR";

/// Build the per-molecule artifact dir for a `(noyau, molecule_id)`
/// pair under the configured `root`.
#[must_use]
pub fn artifact_dir_for(root: &Path, noyau: &str, molecule_id: &str) -> PathBuf {
    root.join(noyau).join(molecule_id)
}

/// One artifact entry on the wire — matches `cosmon-remote`'s
/// `ArtifactEntry` struct field-for-field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEntry {
    /// Filename (basename, no path separator).
    pub name: String,
    /// Detected MIME type (defaults to `application/octet-stream`).
    pub content_type: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Integrity hash of the file contents.
    pub integrity: IntegrityHash,
    /// RFC 3339 creation/modification timestamp.
    pub created_at: String,
    /// Opaque token used to fetch the binary stream.
    pub token: String,
}

/// `{algo, hex}` integrity tuple. `algo` is `"blake3"` for V0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrityHash {
    /// Hash algorithm name. V0 always `"blake3"`.
    pub algo: String,
    /// Lowercase hex digest.
    pub hex: String,
}

/// Top-level manifest body returned by `GET .../artifacts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// Request correlation id (mirrors every other envelope).
    pub request_id: String,
    /// Echo of the molecule id from the URL.
    pub molecule_id: String,
    /// Sorted by `name` for byte-stable output.
    pub artifacts: Vec<ArtifactEntry>,
}

/// Derive the opaque token for a filename. Deterministic over
/// `(molecule_id, name)` so a refetch with no on-disk change returns
/// the same token. Format: `art_<24 chars>` over the lowercase
/// Crockford-base32 alphabet for URL safety.
#[must_use]
pub fn artifact_token(molecule_id: &str, name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(molecule_id.as_bytes());
    hasher.update(b"/");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(28);
    out.push_str("art_");
    out.push_str(&base32_encode(&digest[..15]));
    out
}

/// Crockford-base32 lowercase encoding (24 chars from 15 bytes).
fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buf >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buf << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

/// Map a file extension to a coarse MIME type. Deliberately tiny —
/// the wire schema documents `application/octet-stream` as the
/// fallback so any unknown extension stays opaque.
#[must_use]
pub fn detect_content_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "toml" => "application/toml",
        _ => "application/octet-stream",
    }
}

/// `GET /v1/molecules/{id}/artifacts` — list the artifact manifest.
pub async fn list_artifacts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_artifact_scope(&state, &jwt, "list_artifacts", ARTIFACT_READ)?;

    let spark = build_spark(&state, &jwt, Verb::ListArtifacts, Some(&molecule_id_str))?;
    reject_unsafe_segment(&molecule_id_str, &spark)?;

    let dir = artifact_dir_for(
        state.artifact_root.as_path(),
        spark.noyau.as_str(),
        &molecule_id_str,
    );

    let entries = scan_artifact_dir(&dir, &molecule_id_str).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: e,
        request_id: Some(spark.request_id.clone()),
    })?;

    Ok(Json(json!({
        "request_id": spark.request_id,
        "molecule_id": molecule_id_str,
        "artifacts": entries,
    })))
}

/// `GET /v1/molecules/{id}/artifacts/{token}` — stream binary bytes.
pub async fn fetch_artifact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((molecule_id_str, token)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let bearer = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), bearer, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_artifact_scope(&state, &jwt, "fetch_artifact", ARTIFACT_READ)?;

    let spark = build_spark(&state, &jwt, Verb::FetchArtifact, Some(&molecule_id_str))?;
    reject_unsafe_segment(&molecule_id_str, &spark)?;

    let dir = artifact_dir_for(
        state.artifact_root.as_path(),
        spark.noyau.as_str(),
        &molecule_id_str,
    );

    // Reverse-lookup the token against the directory contents. We
    // iterate rather than parse the token because the token is a
    // SHA-256 prefix of `(molecule_id, name)` — irreversible.
    let entries = scan_artifact_dir(&dir, &molecule_id_str).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        label: e,
        request_id: Some(spark.request_id.clone()),
    })?;
    let matched = entries.iter().find(|e| e.token == token).ok_or(ApiError {
        status: StatusCode::NOT_FOUND,
        label: "artifact_not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    let file_path = dir.join(&matched.name);
    let bytes = tokio::fs::read(&file_path).await.map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "artifact_not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&matched.content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&matched.size_bytes.to_string())
            .unwrap_or(HeaderValue::from_static("0")),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&matched.integrity.hex).unwrap_or(HeaderValue::from_static("")),
    );
    if let Ok(rid) = HeaderValue::from_str(&spark.request_id) {
        response.headers_mut().insert("x-cosmon-request-id", rid);
    }
    Ok(response)
}

/// `PUT /v1/molecules/{id}/artifacts/{name}` — push back-utterance.
///
/// The path parameter is `{token}` in the axum router to share the
/// pattern with `GET .../artifacts/{token}`. On the write side the
/// segment carries the on-disk filename, not a manifest token —
/// the parameter is renamed to `name` inside the handler for clarity.
pub async fn push_artifact(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((molecule_id_str, name)): AxumPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    authorise_artifact_scope(&state, &jwt, "push_artifact", ARTIFACT_WRITE)?;

    let spark = build_spark(&state, &jwt, Verb::PushArtifact, Some(&molecule_id_str))?;
    reject_unsafe_segment(&molecule_id_str, &spark)?;
    reject_unsafe_segment(&name, &spark)?;

    let dir = artifact_dir_for(
        state.artifact_root.as_path(),
        spark.noyau.as_str(),
        &molecule_id_str,
    );
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "artifact_dir_unavailable",
            request_id: Some(spark.request_id.clone()),
        })?;

    // RFC 9530 — `Digest` header carries `blake3=<hex>`. If absent
    // we accept the upload (best-effort) but the response reports
    // the computed hash so the client can verify. If present we
    // verify and reject on mismatch.
    let computed = blake3::hash(&body).to_hex().to_string();
    if let Some(declared) = headers
        .get("digest")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_digest_header)
    {
        if !bool::from(subtle::ConstantTimeEq::ct_eq(
            declared.as_bytes(),
            computed.as_bytes(),
        )) {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                label: "digest_mismatch",
                request_id: Some(spark.request_id.clone()),
            });
        }
    }

    // If-Match: when present, the file at `name` must already exist
    // and its current blake3 digest must equal the supplied ETag.
    // Idempotence: a re-PUT with the right If-Match is a no-op
    // (returns 201), a mismatch is 412.
    let target = dir.join(&name);
    if let Some(expected) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) {
        let trimmed = expected.trim().trim_matches('"');
        let existing = tokio::fs::read(&target).await.ok();
        let actual = existing
            .as_ref()
            .map(|b| blake3::hash(b).to_hex().to_string());
        match actual {
            Some(hex) if hex == trimmed => { /* match — proceed (overwrite ok) */ }
            _ => {
                return Err(ApiError {
                    status: StatusCode::PRECONDITION_FAILED,
                    label: "if_match_failed",
                    request_id: Some(spark.request_id.clone()),
                });
            }
        }
    }

    tokio::fs::write(&target, &body)
        .await
        .map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            label: "artifact_write_failed",
            request_id: Some(spark.request_id.clone()),
        })?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| detect_content_type(&name).to_owned(), str::to_owned);

    let created_at = chrono::Utc::now().to_rfc3339();
    let entry = ArtifactEntry {
        name: name.clone(),
        content_type,
        size_bytes: body.len() as u64,
        integrity: IntegrityHash {
            algo: "blake3".to_owned(),
            hex: computed,
        },
        created_at,
        token: artifact_token(&molecule_id_str, &name),
    };

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "request_id": spark.request_id,
            "artifact": entry,
        })),
    ))
}

/// Walk `dir` and project each regular file to an [`ArtifactEntry`].
/// Missing dir → empty manifest (the molecule may not have produced
/// any artifact yet). Other I/O errors collapse to a static label.
fn scan_artifact_dir(dir: &Path, molecule_id: &str) -> Result<Vec<ArtifactEntry>, &'static str> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(dir).map_err(|_| "artifact_dir_unreadable")?;
    let mut entries: Vec<ArtifactEntry> = Vec::new();
    for child in read.flatten() {
        let path = child.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        // Skip dotfiles — convention for "do not surface".
        if name.starts_with('.') {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let hex = blake3::hash(&bytes).to_hex().to_string();
        let modified = path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map_or_else(|| chrono::Utc::now().to_rfc3339(), |t| t.to_rfc3339());
        entries.push(ArtifactEntry {
            name: name.clone(),
            content_type: detect_content_type(&name).to_owned(),
            size_bytes: bytes.len() as u64,
            integrity: IntegrityHash {
                algo: "blake3".to_owned(),
                hex,
            },
            created_at: modified,
            token: artifact_token(molecule_id, &name),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Reject any segment containing `..` or `/` — defence-in-depth
/// against path traversal even though axum's path-extractor already
/// strips most of it. A failed check is a 400, not a 404 (the input
/// is structurally malformed, not absent).
fn reject_unsafe_segment(segment: &str, spark: &Spark) -> Result<(), ApiError> {
    if segment.is_empty()
        || segment.contains('/')
        || segment.contains('\\')
        || segment.contains("..")
    {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            label: "invalid_path_segment",
            request_id: Some(spark.request_id.clone()),
        });
    }
    Ok(())
}

/// Extract the JWT bearer from the `Authorization` header. Duplicated
/// from `routes::molecules` to keep the two modules independent (the
/// helpers are tiny and the lint suppression that would accompany a
/// shared helper is more friction than the dup).
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

/// Authorise an artifact scope check against the JWT scopes union
/// the admin-nucleon binding-granted scopes. Mirrors
/// `routes::molecules::authorise_scope`.
fn authorise_artifact_scope(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: &'static str,
    wanted: &str,
) -> Result<(), ApiError> {
    use cosmon_state::instrumentation::{emit_authz_decision_with_source, AuthzDecision};

    use crate::auth::scopes::{GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT};

    let nucleon_map = state.nucleon_map.load();
    let binding_scopes = nucleon_map.allowed_scopes_for_audience(&jwt.iss, &jwt.sub, &jwt.aud);
    let (decision, grant_source) = if jwt.has_scope(wanted) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_JWT))
    } else if binding_scopes.iter().any(|b| b == wanted) {
        (AuthzDecision::Allow, Some(GRANT_SOURCE_BINDING))
    } else {
        (AuthzDecision::Absent, None)
    };

    emit_authz_decision_with_source(
        &state.state_dir,
        verb,
        &format!("jwt:{}", jwt.sub),
        Some(wanted),
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

/// Build the admission [`Spark`] common to the three artifact routes.
fn build_spark(
    state: &Arc<AppState>,
    jwt: &ValidatedJwt,
    verb: Verb,
    target: Option<&str>,
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
    http_request_to_spark(&rig, jwt, verb, target)
        .map_err(|e| state.reject_with_request_id(e, new_request_id()))
}

/// Parse an RFC 9530 `Digest` header carrying `blake3=<hex>`.
/// Returns `Some(hex)` on success; `None` otherwise (caller treats
/// absence as "no integrity assertion, server computes its own").
fn parse_digest_header(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (algo, hex) = trimmed.split_once('=')?;
    if !algo.eq_ignore_ascii_case("blake3") {
        return None;
    }
    let hex = hex.trim().trim_matches('"');
    if hex.chars().all(|c| c.is_ascii_hexdigit()) && !hex.is_empty() {
        Some(hex.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_token_is_stable_per_pair() {
        let a = artifact_token("task-1", "haiku.txt");
        let b = artifact_token("task-1", "haiku.txt");
        assert_eq!(a, b);
        assert!(a.starts_with("art_"));
        assert_eq!(a.len(), 28);
    }

    #[test]
    fn artifact_token_differs_per_molecule() {
        let a = artifact_token("task-1", "haiku.txt");
        let b = artifact_token("task-2", "haiku.txt");
        assert_ne!(a, b);
    }

    #[test]
    fn artifact_token_differs_per_name() {
        let a = artifact_token("task-1", "haiku.txt");
        let b = artifact_token("task-1", "report.md");
        assert_ne!(a, b);
    }

    #[test]
    fn token_uses_only_url_safe_chars() {
        let t = artifact_token("task-1", "haiku.txt");
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn detect_content_type_known_extensions() {
        assert_eq!(detect_content_type("haiku.txt"), "text/plain");
        assert_eq!(detect_content_type("REPORT.MD"), "text/plain");
        assert_eq!(detect_content_type("data.json"), "application/json");
        assert_eq!(detect_content_type("graph.png"), "image/png");
        assert_eq!(detect_content_type("no-ext"), "application/octet-stream");
    }

    #[test]
    fn parse_digest_header_accepts_blake3_prefix() {
        assert_eq!(
            parse_digest_header("blake3=deadbeef").as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            parse_digest_header("blake3=\"deadbeef\"").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn parse_digest_header_rejects_other_algos() {
        assert!(parse_digest_header("sha256=deadbeef").is_none());
        assert!(parse_digest_header("no-equals").is_none());
        assert!(parse_digest_header("blake3=not-hex!").is_none());
    }

    #[test]
    fn artifact_dir_compose() {
        let d = artifact_dir_for(Path::new("/tmp/cosmon"), "tenant-demo", "task-1");
        assert_eq!(d, PathBuf::from("/tmp/cosmon/tenant-demo/task-1"));
    }

    #[test]
    fn manifest_entry_serializes_to_expected_shape() {
        let e = ArtifactEntry {
            name: "haiku.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            size_bytes: 42,
            integrity: IntegrityHash {
                algo: "blake3".to_owned(),
                hex: "deadbeef".to_owned(),
            },
            created_at: "2026-05-22T11:00:00+00:00".to_owned(),
            token: "art_aaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["name"], "haiku.txt");
        assert_eq!(v["content_type"], "text/plain");
        assert_eq!(v["size_bytes"], 42);
        assert_eq!(v["integrity"]["algo"], "blake3");
        assert_eq!(v["integrity"]["hex"], "deadbeef");
    }

    #[test]
    fn scan_missing_dir_returns_empty() {
        let entries = scan_artifact_dir(Path::new("/nonexistent/smithy/x"), "task-1").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn scan_lists_regular_files_and_sorts() {
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("b.txt"), b"two").unwrap();
        std::fs::write(td.path().join("a.txt"), b"one").unwrap();
        std::fs::write(td.path().join(".hidden"), b"skipped").unwrap();
        std::fs::create_dir_all(td.path().join("subdir")).unwrap();
        let entries = scan_artifact_dir(td.path(), "task-1").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "b.txt");
        assert_eq!(entries[0].size_bytes, 3);
        assert_eq!(entries[0].integrity.algo, "blake3");
    }

    #[test]
    fn base32_encode_is_lowercase_and_correct_length() {
        let out = base32_encode(&[0xff; 15]);
        assert_eq!(out.len(), 24);
        assert!(out.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(out
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }
}
