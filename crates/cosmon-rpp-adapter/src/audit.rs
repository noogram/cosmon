// SPDX-License-Identifier: AGPL-3.0-only

//! Causal-closure inbox materialisation — clause (b) of the §8j
//! HTTPS+JWT instantiation (ADR-080 §3.2).
//!
//! Every admitted request lands as
//! `<inbox_root>/<request_id>.json` **before** `cs` is invoked. The
//! file carries the JWT claim digest (never the raw token), the
//! resolved nucleon, and the request envelope. Downstream `cs`
//! processes (or `cs reconcile`) read these files back; the RPP
//! never writes to `.cosmon/state/` directly.
//!
//! # Anti-leak discipline
//!
//! - Raw JWT bytes never reach disk.
//! - The audit JSON shape is `{ "claims": { "iss", "sub_hash",
//!   "aud", "iat", "exp", "jti" }, "nucleon_id", "verb", "request_id",
//!   "received_at" }` — no `Authorization` header capture, no body
//!   echo for routes that have one.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::RppRejectReason;
use crate::jwt::ValidatedJwt;
use crate::nucleon_map::{HabilitationId, Resolved};
use crate::rate_limit::hash_sub;

/// Generate a fresh request identifier (V0: timestamp + 8 random bytes
/// in hex). Stable enough for audit cross-reference; not a secret.
#[must_use]
pub fn new_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut buf = [0u8; 8];
    getrandom_or_zero(&mut buf);
    let mut hex = String::with_capacity(buf.len() * 2);
    for b in &buf {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("req-{now_ms:x}-{hex}")
}

fn getrandom_or_zero(buf: &mut [u8]) {
    // Cheap entropy without pulling `rand` — `/dev/urandom` is good
    // enough for an opaque request id (not a token, not a secret).
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(buf);
    }
}

/// Audit envelope written to `<inbox_root>/<request_id>.json`.
#[derive(Debug, Serialize)]
pub struct AuditRecord {
    /// Request id (also the file's stem).
    pub request_id: String,
    /// ISO-8601 UTC.
    pub received_at: String,
    /// Resolved nucleon (clause a).
    pub nucleon_id: String,
    /// Tenant scope.
    pub noyau: String,
    /// Verb dispatched on the cs subprocess (e.g. `"observe"`).
    pub verb: String,
    /// Specific molecule id, when applicable.
    pub molecule_id: Option<String>,
    /// JWT claim digest — never the raw token.
    pub claims: ClaimDigest,
}

/// Sanitised JWT claims kept in the inbox file.
#[derive(Debug, Serialize)]
pub struct ClaimDigest {
    /// `iss`.
    pub iss: String,
    /// BLAKE3 hex of the JWT `sub` (the raw `sub` is *not* persisted —
    /// turing G9).
    pub sub_hash: String,
    /// `aud`.
    pub aud: String,
    /// `jti`.
    pub jti: String,
    /// JWT lifetime (`exp - iat`), seconds.
    pub lifetime_sec: u64,
}

/// Materialise the audit record on disk before any `cs` invocation.
///
/// # Errors
///
/// Returns [`RppRejectReason::InboxMaterializationFailed`] if the
/// directory cannot be created or the file cannot be written.
pub fn materialize(
    inbox_root: &Path,
    request_id: &str,
    jwt: &ValidatedJwt,
    resolved: &Resolved,
    verb: &str,
    molecule_id: Option<&str>,
) -> Result<PathBuf, RppRejectReason> {
    let dir = inbox_root.join("api");
    std::fs::create_dir_all(&dir)
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    let received_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let record = AuditRecord {
        request_id: request_id.to_owned(),
        received_at,
        nucleon_id: resolved.nucleon_id.0.clone(),
        noyau: resolved.noyau.0.clone(),
        verb: verb.to_owned(),
        molecule_id: molecule_id.map(str::to_owned),
        claims: ClaimDigest {
            iss: jwt.iss.clone(),
            sub_hash: hash_sub(&jwt.sub),
            aud: jwt.aud.clone(),
            jti: jwt.jti.clone(),
            lifetime_sec: jwt.lifetime_sec,
        },
    };
    let path = dir.join(format!("{request_id}.json"));
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    std::fs::write(&path, bytes)
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    Ok(path)
}

/// Verify the audit file does not contain the raw `sub` or the raw
/// JWT — used by the *no-leak* test in
/// `tests/admission_test.rs`.
#[must_use]
pub fn assert_no_leak(record_bytes: &[u8], raw_sub: &str, raw_token: &str) -> bool {
    let s = std::str::from_utf8(record_bytes).unwrap_or("");
    !s.contains(raw_sub) && !s.contains(raw_token)
}

/// Convenience wrapper that resolves a [`HabilitationId`] and writes a
/// minimal audit envelope without a JWT (used by tests / rehearsal).
#[doc(hidden)]
pub fn materialize_basic(
    inbox_root: &Path,
    request_id: &str,
    nucleon_id: &HabilitationId,
    verb: &str,
) -> Result<PathBuf, RppRejectReason> {
    let dir = inbox_root.join("api");
    std::fs::create_dir_all(&dir)
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    let path = dir.join(format!("{request_id}.json"));
    let body = serde_json::json!({
        "request_id": request_id,
        "nucleon_id": nucleon_id.as_str(),
        "verb": verb,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap())
        .map_err(|e| RppRejectReason::InboxMaterializationFailed(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_has_req_prefix() {
        let id = new_request_id();
        assert!(id.starts_with("req-"));
    }

    #[test]
    fn materialize_writes_under_inbox_api() {
        let td = tempfile::TempDir::new().unwrap();
        let id = "req-abc";
        let resolved = Resolved {
            nucleon_id: HabilitationId::new("nuc-a"),
            noyau: crate::nucleon_map::Noyau::new("tenant-demo"),
            audience: "cosmon-rpp-tenant".into(),
            allowed_scopes: Vec::new(),
            drain_bounds: crate::nucleon_map::DrainBounds::default(),
            federated: None,
        };
        let jwt = ValidatedJwt {
            iss: "https://idp".into(),
            sub: "sub-123".into(),
            aud: "cosmon-rpp-tenant".into(),
            jti: "tok-1".into(),
            lifetime_sec: 60,
            exp: 9_999_999_999,
            scopes: Vec::new(),
        };
        let path = materialize(td.path(), id, &jwt, &resolved, "observe", Some("mol-1")).unwrap();
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        // Raw `sub` MUST NOT leak — only the BLAKE3 hash lands.
        assert!(!text.contains("sub-123"));
        assert!(text.contains(&hash_sub("sub-123")));
        assert!(text.contains("nuc-a"));
        assert!(text.contains("tenant-demo"));
        assert!(text.contains("observe"));
        assert!(text.contains("mol-1"));
    }
}
