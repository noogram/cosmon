// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /cluster` — expose the machine-level cluster topology file so
//! that native pilots (iOS, Mac, future devices) can self-configure
//! without hardcoding Tailscale IPs.
//!
//! See [ADR-066](../../../docs/adr/066-surfaces-cluster-config.md).
//!
//! When the file is missing the endpoint returns **HTTP 200** with body
//! `{"error":"not_configured"}`. The iOS pilot treats that shape as
//! "fall back to the compile-time default" — a 500 would force the
//! client to handle a transport error where the semantic answer is
//! simply "file not seeded yet".
//!
//! NOTE: the handler is not yet wired into the Axum router. The module
//! is kept in-tree (and declared so `cs` binary compilation can resolve
//! sibling items like `AppState::with_cluster_config_path`) while the
//! route registration ships in a follow-up. `dead_code` is expected
//! until then.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use cosmon_core::cluster::ClusterConfig;
use serde_json::Value;

use crate::{ApiError, AppState};

/// `GET /cluster` handler.
pub async fn get_cluster(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let path = resolve_cluster_config_path(&state);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let parsed = ClusterConfig::from_toml_str(&raw).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("parse cluster.toml at {}: {e}", path.display()),
                )
            })?;
            let value = serde_json::to_value(&parsed).map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("serialize cluster config: {e}"),
                )
            })?;
            Ok(Json(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Json(serde_json::json!({
            "error": "not_configured",
            "path": path.to_string_lossy(),
        }))),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read {}: {e}", path.display()),
        )),
    }
}

/// Resolve the file path for this request: explicit override in
/// `AppState` wins, then `$COSMON_CLUSTER_CONFIG`, then
/// `$HOME/.config/cosmon/cluster.toml`.
pub(crate) fn resolve_cluster_config_path(state: &AppState) -> PathBuf {
    if let Some(p) = state.cluster_config_path.as_ref() {
        return p.clone();
    }
    if let Ok(p) = std::env::var("COSMON_CLUSTER_CONFIG") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("cosmon")
        .join("cluster.toml")
}
