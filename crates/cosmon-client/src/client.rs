// SPDX-License-Identifier: Apache-2.0

//! HTTP client for the cosmon-saas API.
//!
//! Every method maps to a single route on the server and returns either
//! a typed deserialized response, or a structured error that the CLI can
//! render for the operator.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

use crate::config::ClientConfig;

/// A ready-to-use HTTP client. Cheap to clone (wraps an Arc internally).
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

/// Matches the body returned by `POST /api/v1/nucleate`.
#[derive(Debug, Deserialize)]
pub struct NucleateResponse {
    /// Canonical molecule ID (e.g. `task-20260420-a1b2`).
    pub molecule_id: Option<String>,
    /// Some formulas echo a different field name — keep the raw JSON too.
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// A subset of the server's `observe` projection useful for CLI rendering.
#[derive(Debug, Deserialize)]
pub struct MoleculeState {
    pub id: Option<String>,
    pub formula: Option<String>,
    pub status: Option<String>,
    pub current_step: Option<usize>,
    pub total_steps: Option<usize>,
    pub worker: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

/// Shape of `GET /api/v1/artifacts/:id/list`.
#[derive(Debug, Deserialize, Serialize)]
pub struct ArtifactListing {
    pub molecule_id: String,
    pub files: Vec<ArtifactEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ArtifactEntry {
    pub path: String,
    pub bytes: u64,
}

impl Client {
    /// Build a client from [`ClientConfig`]. Sets a 30-second default timeout
    /// so the CLI never hangs forever on a half-open tunnel.
    pub fn new(cfg: &ClientConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base: cfg.server.trim_end_matches('/').to_owned(),
            api_key: cfg.api_key.clone(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// `GET /healthz` — unauthenticated probe.
    pub async fn healthz(&self) -> anyhow::Result<serde_json::Value> {
        let resp = self.http.get(self.url("/healthz")).send().await?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("healthz failed ({status}): {body}");
        }
        Ok(body)
    }

    /// `POST /api/v1/nucleate`.
    pub async fn nucleate(
        &self,
        formula: &str,
        variables: &BTreeMap<String, String>,
        tags: &[String],
        blocked_by: &[String],
    ) -> anyhow::Result<NucleateResponse> {
        let body = serde_json::json!({
            "formula": formula,
            "variables": variables,
            "tags": tags,
            "blocked_by": blocked_by,
        });
        let resp = self
            .http
            .post(self.url("/api/v1/nucleate"))
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("nucleate failed ({status}): {text}");
        }
        let parsed: NucleateResponse =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{e}: {text}"))?;
        Ok(parsed)
    }

    /// `POST /api/v1/tackle/:id`.
    pub async fn tackle(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v1/tackle/{id}")))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        json_or_error(resp).await
    }

    /// `GET /api/v1/observe/:id`.
    pub async fn observe(&self, id: &str) -> anyhow::Result<MoleculeState> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/observe/{id}")))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("observe failed ({status}): {text}");
        }
        let parsed: MoleculeState =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{e}: {text}"))?;
        Ok(parsed)
    }

    /// Poll `observe` until the molecule reaches a terminal state.
    pub async fn wait(&self, id: &str, poll_interval: Duration) -> anyhow::Result<MoleculeState> {
        loop {
            let state = self.observe(id).await?;
            if is_terminal(state.status.as_deref()) {
                return Ok(state);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// `POST /api/v1/done/:id`.
    pub async fn done(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let resp = self
            .http
            .post(self.url(&format!("/api/v1/done/{id}")))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        json_or_error(resp).await
    }

    /// `GET /api/v1/artifacts/:id/list`.
    pub async fn list_artifacts(&self, id: &str) -> anyhow::Result<ArtifactListing> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/artifacts/{id}/list")))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("list_artifacts failed ({status}): {text}");
        }
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{e}: {text}"))
    }

    /// `GET /api/v1/artifacts/:id` — downloads the tar.gz and extracts under
    /// `dest_dir/<molecule_id>/…`. Returns the destination directory.
    pub async fn fetch_artifacts(&self, id: &str, dest_dir: &Path) -> anyhow::Result<PathBuf> {
        let resp = self
            .http
            .get(self.url(&format!("/api/v1/artifacts/{id}")))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("fetch_artifacts failed ({status}): {text}");
        }
        let bytes = resp.bytes().await?;
        std::fs::create_dir_all(dest_dir)?;
        let decoder = GzDecoder::new(std::io::Cursor::new(&bytes));
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(false);
        archive.unpack(dest_dir)?;
        Ok(dest_dir.join(id))
    }
}

async fn json_or_error(resp: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("request failed ({status}): {text}");
    }
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{e}: {text}"))
}

/// Statuses `cs observe` may report as "work is over".
fn is_terminal(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "collapsed" | "frozen" | "merged")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_terminal_recognizes_standard_states() {
        assert!(is_terminal(Some("completed")));
        assert!(is_terminal(Some("collapsed")));
        assert!(!is_terminal(Some("active")));
        assert!(!is_terminal(Some("pending")));
        assert!(!is_terminal(None));
    }
}
