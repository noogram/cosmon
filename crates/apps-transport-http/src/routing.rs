// SPDX-License-Identifier: Apache-2.0

//! URL versioning helpers — `/v1/<resource>`.
//!
//! The crate's wire convention is that every public resource lives under
//! a `/v1/` prefix. This is the smallest stable contract that Swift
//! clients can count on; bumping the prefix to `/v2/` is the protocol's
//! semver lever.
//!
//! Use [`v1::path`] to format paths consistently rather than hand-typing
//! the prefix in each handler/test.
//!
//! Convention:
//! - `/v1/health` — liveness probe (`{"ok": true, "service": "...", ...}`)
//! - `/v1/<resource>` — namespaced resources.
//!
//! No code-generation, no procedural macros. The discipline is editorial.

pub mod v1 {
    /// Build a `/v1/<resource>` path. `resource` may have leading `/` or
    /// not; both are normalized.
    #[must_use]
    pub fn path(resource: &str) -> String {
        let trimmed = resource.trim_start_matches('/');
        if trimmed.is_empty() {
            "/v1".to_string()
        } else {
            format!("/v1/{trimmed}")
        }
    }

    /// Convention path for the daemon liveness probe.
    pub const HEALTH: &str = "/v1/health";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_path_normalizes_leading_slash() {
        assert_eq!(v1::path("pins"), "/v1/pins");
        assert_eq!(v1::path("/pins"), "/v1/pins");
        assert_eq!(v1::path("/pins/foo"), "/v1/pins/foo");
    }

    #[test]
    fn v1_path_handles_empty_resource() {
        assert_eq!(v1::path(""), "/v1");
        assert_eq!(v1::path("/"), "/v1");
    }
}
