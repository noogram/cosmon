// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /dist/binary/{platform}/cosmon-remote` — serve the
//! pre-built `cosmon-remote` CLI for the four cross-compile targets
//! (Phase 1 dist multi-OS).
//!
//! # Why this exists
//!
//! Phase 0 served a justfile (`GET /dist/justfile`) so tenants could
//! drive the API via `just` recipes. Phase 1 ships a real Rust CLI
//! (`crates/cosmon-remote`) cross-compiled to the four common
//! platforms. The route here is the §8 entry point: the tenant's
//! `install.sh` detects `uname`, picks a platform, downloads the
//! binary, drops it in `~/.local/bin/cosmon-remote`.
//!
//! # Allow-list, not free-form
//!
//! The `{platform}` path segment is validated against
//! [`KNOWN_PLATFORMS`]. Anything outside the list returns 404 —
//! refusing to interpret the segment as a filesystem path forbids
//! traversal (`..`, absolute paths) by construction.
//!
//! # Operational class
//!
//! Like `/healthz`, `/install.sh`, `/dist/justfile` and the root
//! landing, this route is *operational*: unauthenticated, outside
//! `/v1/`, and intentionally excluded from the §8p frozen API
//! surface. The binary it serves carries no secret; the JWT
//! issuance that gives a tenant access to molecules happens later,
//! after the binary has been downloaded and the operator has run
//! `cosmon-remote auth login`.
//!
//! # Source of the bytes
//!
//! [`DistState::root`] is the directory the four per-platform
//! subdirectories live under. The image build COPYs them in at
//! `/opt/cosmon-remote/dist/` (see the adapter Dockerfile *and* the
//! smithy `Dockerfile.cosmon-server` used for the Harbor bakes —
//! both COPY to the same canonical path); on a host running the
//! adapter outside Docker the operator can repoint via
//! `RppConfig.dist_root`.
//!
//! # One canonical path (Pierre v1.5 retour)
//!
//! [`DEFAULT_DIST_ROOT`] is `/opt/cosmon-remote/dist` — the *same*
//! path every Dockerfile COPYs the binaries to. Before this fix the
//! default was `/usr/local/share/cosmon-remote/binaries` while the
//! production image (smithy `Dockerfile.cosmon-server`) baked the
//! binaries to `/opt/cosmon-remote/dist` and relied on a `dist_root`
//! override in `rpp.toml` to bridge the gap. Any deployment that
//! dropped or never set that override fell back to the empty default
//! and answered `404` on every `/dist/binary/...` request — breaking
//! `curl install.sh | sh` for every tenant. Aligning the default with
//! the COPY path makes the override redundant (belt-and-braces, not
//! load-bearing) so a missing override can no longer 404.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// Default on-disk root where the four per-platform subdirectories
/// live. Every Dockerfile that ships the adapter COPYs the host-built
/// binaries to this exact path (the cosmon-rpp-adapter Dockerfile and
/// the smithy `Dockerfile.cosmon-server` used for Harbor bakes), so
/// the image serves binaries with no `rpp.toml` override required.
///
/// This was moved from
/// `/usr/local/share/cosmon-remote/binaries` to `/opt/cosmon-remote/dist`
/// to match the production COPY path; see the module docs for why a
/// mismatched default 404'd `curl install.sh | sh` for tenants. A guard
/// test (`dist_serving::default_dist_root_is_canonical_opt_path`) pins
/// this constant so the alignment can't silently drift again.
pub const DEFAULT_DIST_ROOT: &str = "/opt/cosmon-remote/dist";

/// Allow-list of `{platform}` segment values the route accepts. Any
/// other value yields 404 — the dispatch never reaches the filesystem
/// with an attacker-controlled path component.
pub const KNOWN_PLATFORMS: &[&str] = &["macos-arm64", "macos-amd64", "linux-arm64", "linux-amd64"];

/// The URL path the binary-serving route exposes for a given platform
/// component — the **single Rust-side source** of the path layout the
/// `install.sh` shell script must build to fetch a binary
/// (`$COSMON_HOST` + this path). The route registration in `lib.rs`
/// derives its pattern from this same function (`binary_url_path
/// ("{platform}")`), so there is exactly one Rust statement of the
/// layout; the served script is checked against it by the snapshot
/// test `dist_serving::install_sh_paths_match_dist_route`.
///
/// # Why a function and not a `const` the shell reads
///
/// The shell script runs via `curl … | sh` on a remote host with no
/// cosmon binary and no Rust toolchain — "the socket is a type-system
/// event horizon". No shared type can
/// reach across that boundary, so the two languages cannot share a
/// `const`. The snapshot test is the cheapest possible substitute for
/// the type: it re-derives the served script against this function and
/// fails at `cargo test` iff a human edits the shell URL (install.sh
/// `~line 73`) without editing this route, or vice versa. It pins a
/// generated artefact against its own source (regenerate-and-diff), it
/// is **not** a runtime checker reconciling two independently-maintained
/// authorities — there is no validator, daemon, or registry in the
/// running system (godel, same deliberation).
///
/// `platform` is interpolated verbatim: pass a concrete platform name
/// (`"linux-amd64"`) for the per-platform URL, or the shell variable
/// token (`"$PLATFORM"`) to obtain the template the script assembles at
/// runtime after `uname` selects a platform.
#[must_use]
pub fn binary_url_path(platform: &str) -> String {
    format!("/dist/binary/{platform}/cosmon-remote")
}

/// Per-request state for the dist route. The struct exists so future
/// additions (caching headers, integrity hashes, alt-host mirror) sit
/// on a typed surface rather than a free-form `PathBuf` in
/// [`AppState`].
#[derive(Clone, Debug)]
pub struct DistState {
    /// On-disk root directory. The route reads
    /// `<root>/<platform>/cosmon-remote`.
    pub root: PathBuf,
}

impl DistState {
    /// Build a [`DistState`] at the given root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the on-disk path for a known platform. Returns
    /// `None` for any platform not in [`KNOWN_PLATFORMS`].
    #[must_use]
    pub fn binary_path(&self, platform: &str) -> Option<PathBuf> {
        if KNOWN_PLATFORMS.contains(&platform) {
            Some(self.root.join(platform).join("cosmon-remote"))
        } else {
            None
        }
    }
}

/// `GET /dist/binary/{platform}/cosmon-remote` — stream the
/// cross-compiled binary for the requested platform.
///
/// Operational, unauthenticated, outside `/v1/`.
///
/// Status codes:
///
/// - **200** — binary streamed with
///   `Content-Type: application/octet-stream` and
///   `Content-Disposition: attachment; filename="cosmon-remote"`.
/// - **404** — `{platform}` not in [`KNOWN_PLATFORMS`], or the
///   on-disk file is missing (image was built without binaries).
pub async fn serve_binary(
    AxumPath(platform): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(path) = state.dist.binary_path(&platform) else {
        return not_found(&format!(
            "unknown platform {platform:?} — known: {}",
            KNOWN_PLATFORMS.join(", ")
        ));
    };
    stream_file(&path).await
}

async fn stream_file(path: &Path) -> Response {
    // 6 MB binaries fit comfortably in memory; the simple read-then-respond
    // form matches `routes::artifacts::fetch_artifact` and avoids dragging
    // in `tokio-util` for a single small payload.
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return not_found(&format!(
                "binary not found on adapter — image built without `just dist-binaries`? ({})",
                path.display()
            ));
        }
        Err(err) => {
            return internal(&format!("read {}: {err}", path.display()));
        }
    };
    let size = bytes.len();

    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"cosmon-remote\""),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    resp
}

fn not_found(msg: &str) -> Response {
    (StatusCode::NOT_FOUND, msg.to_owned()).into_response()
}

fn internal(msg: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_owned()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_platforms_accept() {
        let s = DistState::new("/tmp/dist");
        for p in KNOWN_PLATFORMS {
            assert!(s.binary_path(p).is_some(), "platform {p} rejected");
        }
    }

    #[test]
    fn unknown_platform_rejected() {
        let s = DistState::new("/tmp/dist");
        assert!(s.binary_path("freebsd-arm64").is_none());
        assert!(s.binary_path("../etc/passwd").is_none());
        assert!(s.binary_path("").is_none());
    }

    #[test]
    fn binary_path_layout() {
        let s = DistState::new("/srv/binaries");
        let p = s.binary_path("linux-amd64").unwrap();
        assert_eq!(p, PathBuf::from("/srv/binaries/linux-amd64/cosmon-remote"));
    }

    #[test]
    fn binary_url_path_layout() {
        // Per-platform URL the route serves.
        assert_eq!(
            binary_url_path("linux-amd64"),
            "/dist/binary/linux-amd64/cosmon-remote"
        );
        // Template form (what the shell builds at runtime) reuses the
        // identical layout, so route + script flow from one source.
        assert_eq!(
            binary_url_path("$PLATFORM"),
            "/dist/binary/$PLATFORM/cosmon-remote"
        );
    }
}
