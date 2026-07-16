// SPDX-License-Identifier: AGPL-3.0-only

//! Phone-home ingest — materialise the CLI's passive opt-out remontée.
//!
//! The tenant CLI, on an abandonment-predicting failure (503, 502,
//! 4xx burst), queues `request_id + error code` and lets the pair ride
//! its **next** request as the `X-Cosmon-Phone-Home` header (one line
//! shown to the client; `config set phone-home off` cuts it —
//! D-AVATAR-1, the client cuts, the instance never imposes).
//!
//! This layer is the receiving half: no new route (§8p untouched), a
//! middleware that mirrors [`crate::routes::quota::rate_limit_headers_layer`]
//! — it reads the header off authenticated requests, re-validates the
//! JWT against the sealed JWKS, resolves the noyau, and writes one
//! report file per pair under `<inbox_root>/phone-home/<request_id>.json`,
//! next to the audit envelopes where `cs patrol --abandon` reads it.
//!
//! # Anti-leak discipline
//!
//! Same contract as [`crate::audit`]: the report carries the BLAKE3
//! `sub_hash` (never the raw `sub`), the sanitised `request_id` and
//! error code (charset-gated, length-capped — also the path-traversal
//! guard, since the request id becomes a file stem), the noyau, and a
//! timestamp. Nothing else from the request ever lands in the file.

use std::path::Path;
use std::sync::Arc;

use crate::jwt::JwtVerifier;
use crate::rate_limit::hash_sub;
use crate::AppState;

/// Wire header carrying `rid:code[,rid:code…]` pairs from the CLI.
pub const PHONE_HOME_HEADER: &str = "x-cosmon-phone-home";

/// Hard cap on pairs ingested from a single header — mirrors the CLI's
/// send cap; anything beyond is dropped, not an error.
pub const MAX_REPORTS_PER_HEADER: usize = 8;

/// Maximum length of each token after sanitisation.
const MAX_TOKEN_LEN: usize = 64;

/// Charset gate: keep `[A-Za-z0-9._-]`, cap the length, reject empty.
/// This doubles as the path-traversal guard — the request id becomes
/// the report's file stem and can never contain a separator.
#[must_use]
pub fn sanitize_token(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
        .take(MAX_TOKEN_LEN)
        .collect();
    // A token of only dots could still walk (`..`) — refuse anything
    // that is not at least one alphanumeric.
    if cleaned.chars().any(|c| c.is_ascii_alphanumeric()) {
        Some(cleaned)
    } else {
        None
    }
}

/// Parse a header value into sanitised `(request_id, error_code)`
/// pairs. Pairs that do not survive sanitisation are dropped silently
/// — the remontée is best-effort by contract.
#[must_use]
pub fn parse_header(value: &str) -> Vec<(String, String)> {
    value
        .split(',')
        .take(MAX_REPORTS_PER_HEADER)
        .filter_map(|pair| {
            let (rid, code) = pair.split_once(':')?;
            let code = sanitize_token(code)?;
            // `-` is the CLI's "no request id" placeholder.
            let rid = sanitize_token(rid).unwrap_or_else(|| "unknown".to_owned());
            Some((rid, code))
        })
        .collect()
}

/// Write one report file per pair under `<inbox_root>/phone-home/`.
/// Returns how many landed. Best-effort: IO failures drop the report,
/// never the carrying request.
#[must_use]
pub fn materialize_reports(
    inbox_root: &Path,
    noyau: &str,
    sub_hash: &str,
    pairs: &[(String, String)],
    reported_at: &str,
) -> usize {
    let dir = inbox_root.join("phone-home");
    if std::fs::create_dir_all(&dir).is_err() {
        return 0;
    }
    let mut written = 0;
    for (rid, code) in pairs {
        let body = serde_json::json!({
            "reported_request_id": rid,
            "error_code": code,
            "noyau": noyau,
            "sub_hash": sub_hash,
            "reported_at": reported_at,
        });
        let path = dir.join(format!("{rid}.json"));
        if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
            if std::fs::write(&path, bytes).is_ok() {
                written += 1;
            }
        }
    }
    written
}

/// Axum middleware: ingest the phone-home header off authenticated
/// requests. Mirrors the rate-limit header layer — snapshot what we
/// need, run the request untouched, never fail it.
pub async fn phone_home_ingest_layer(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let header_value: Option<String> = req
        .headers()
        .get(PHONE_HOME_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let token: Option<String> = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(|t| t.trim().to_owned());

    if let (Some(value), Some(t)) = (header_value, token) {
        if let Ok(jwt) = JwtVerifier::validate(&state.jwks.load(), &t, state.posture) {
            let map = state.nucleon_map.load();
            if let Some(resolved) = map.resolve(&jwt.iss, &jwt.sub) {
                let pairs = parse_header(&value);
                if !pairs.is_empty() {
                    let reported_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    let _ = materialize_reports(
                        &state.inbox_root,
                        resolved.noyau.as_str(),
                        &hash_sub(&jwt.sub),
                        &pairs,
                        &reported_at,
                    );
                }
            }
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_happy_path() {
        let pairs = parse_header("req-1:503_tackle_unavailable,req-2:409_reserved_name");
        assert_eq!(
            pairs,
            vec![
                ("req-1".to_owned(), "503_tackle_unavailable".to_owned()),
                ("req-2".to_owned(), "409_reserved_name".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_header_caps_pair_count() {
        let value = (0..20)
            .map(|i| format!("req-{i}:503_x"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_header(&value).len(), MAX_REPORTS_PER_HEADER);
    }

    // The request id becomes a file stem — traversal must be impossible.
    #[test]
    fn sanitize_blocks_path_traversal() {
        assert_eq!(
            sanitize_token("../../etc/passwd").as_deref(),
            Some("....etcpasswd")
        );
        assert!(sanitize_token("..").is_none());
        assert!(sanitize_token("/").is_none());
        assert!(sanitize_token("").is_none());
        let pairs = parse_header("../../x:503_y");
        assert_eq!(pairs.len(), 1);
        assert!(!pairs[0].0.contains('/'));
    }

    // Gate anti-fuite: whatever rides the header, the report on disk
    // carries only sanitised id + code + hashed sub + noyau.
    #[test]
    fn report_never_contains_raw_sub_or_free_text() {
        let td = tempfile::TempDir::new().unwrap();
        let raw_sub = "tenant-demo-operator";
        let pairs = parse_header("req-77:503_tackle_unavailable,evil:contenu du casier!");
        let n = materialize_reports(
            td.path(),
            "tenant-demo",
            &hash_sub(raw_sub),
            &pairs,
            "2026-06-11T00:00:00Z",
        );
        assert_eq!(n, 2);
        for entry in std::fs::read_dir(td.path().join("phone-home")).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains(raw_sub), "raw sub leaked: {text}");
            assert!(text.contains(&hash_sub(raw_sub)));
            // Every persisted field is charset-bounded — free prose
            // ("contenu du casier!") cannot survive sanitisation intact.
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            for key in ["reported_request_id", "error_code"] {
                let field = v[key].as_str().unwrap();
                assert!(
                    field
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)),
                    "unsanitised content in {key}: {field}"
                );
            }
        }
    }

    #[test]
    fn materialize_writes_expected_shape() {
        let td = tempfile::TempDir::new().unwrap();
        let pairs = vec![("req-abc".to_owned(), "502_token_exchange".to_owned())];
        let n = materialize_reports(
            td.path(),
            "tenant-demo",
            "deadbeef",
            &pairs,
            "2026-06-11T00:00:00Z",
        );
        assert_eq!(n, 1);
        let text =
            std::fs::read_to_string(td.path().join("phone-home").join("req-abc.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["reported_request_id"], "req-abc");
        assert_eq!(v["error_code"], "502_token_exchange");
        assert_eq!(v["noyau"], "tenant-demo");
        assert_eq!(v["sub_hash"], "deadbeef");
        assert_eq!(v["reported_at"], "2026-06-11T00:00:00Z");
    }
}
