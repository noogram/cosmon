// SPDX-License-Identifier: AGPL-3.0-only

//! The RPP's dispatch **preconditions** — the server-side implementation
//! of [`cosmon_runtime::SpawnPreflight`] (issue #48, restored on the
//! library seam by task-20260911-be1e).
//!
//! # What this module is for
//!
//! `POST /v1/molecules/{id}/tackle` publishes three stable `503`
//! identifiers as a wire contract, and an external reporter consumes them
//! by name:
//!
//! | label | meaning | raised by |
//! |---|---|---|
//! | `worker_credential_missing` | no credential the worker could use | this module |
//! | `adapter_backend_unreachable` | the adapter's backend cannot serve | this module |
//! | `subprocess_spawn_failed` | the worker process could not be started | the transport, via [`cosmon_runtime::TackleExecError::Spawn`] |
//!
//! The first two are *preconditions*: knowable before the dispatch spends
//! anything, with different repairs. Until issue #54 U6 they were raised
//! inside `cs tackle` and recovered by the adapter through stderr
//! substring matching. The library cut-over removed the stderr — and with
//! it the checks, which had never been ported. A `tackle` with no
//! credential then answered `200` and a receipt: the worker booted, sat on
//! `Not logged in · Run /login`, and read as healthy to every liveness
//! probe cosmon has. Measured on both arches at the v3.10 bake
//! (2026-09-10) against `c2c8ba61`, with `de97ff2d` (v3.9) answering `503
//! worker_credential_missing` for the identical request.
//!
//! # Why the checks are *not* simply copied from the CLI
//!
//! `cs tackle` resolves a credential through the **operator's ambient
//! environment**. That is right for a terminal and wrong for a
//! multi-tenant server twice over:
//!
//! 1. The adapter's own environment is not the worker's. The worker is
//!    spawned through [`crate::worker_env::EnvelopedBackend`], whose
//!    allow-list ([`crate::worker_env::PASSTHROUGH_VARS`]) carries neither
//!    `CLAUDE_CONFIG_DIR` nor `CLAUDE_CODE_OAUTH_TOKEN`. A check that read
//!    the adapter's copy of either would pass a dispatch whose worker will
//!    never see it — the false *green* that mirrors the false 200.
//!    [`RppSpawnPreflight`] therefore asks the question against
//!    the **enveloped** environment, compiled by the very function that
//!    builds the spawn's env.
//! 2. The backend a tenant's `local` adapter dials is a property of the
//!    tenant's project config, not of the server process. The backend
//!    probe here reads `[adapters.<name>].base_url` and nothing else —
//!    deliberately *narrower* than the CLI's `COSMON_LOCAL_BASE_URL` /
//!    `OLLAMA_HOST` / `OPENAI_BASE_URL` chain, for the same reason
//!    [`crate::worker_env::tenant_state_dir`] refuses the ambient
//!    `COSMON_STATE_DIR`: one inherited value would silently redirect
//!    every tenant.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cosmon_runtime::{PreflightContext, PreflightRefusal, SpawnPreflight};

use crate::auth_claude::credentials::classify_credentials_file;
use crate::worker_env::WorkerEnvelope;

/// Adapter names that spawn an interactive Claude Code worker, and whose
/// dispatch therefore needs a usable Claude credential.
const CLAUDE_ADAPTERS: &[&str] = &["claude"];

/// Adapter names that dial a local OpenAI-compatible backend.
const LOCAL_ADAPTERS: &[&str] = &["local", "ollama"];

/// Wall-clock budget for the backend probe.
///
/// Short on purpose: this sits on the request path, and an unreachable
/// backend must surface as a refusal rather than as a stalled HTTP
/// request that the tenant's client times out on with no label at all.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The RPP's preconditions, evaluated per dispatch.
///
/// Holds the tenant-independent half of what the checks need; the
/// tenant-dependent half (project config, worker envelope) is carried per
/// dispatch because one adapter process serves many noyaux.
#[derive(Debug, Clone)]
pub struct RppSpawnPreflight {
    /// The envelope this dispatch's worker will be spawned under — the
    /// only truthful source for "what environment will the worker read".
    envelope: WorkerEnvelope,
    /// The admitted tenant's project config, for the backend probe's
    /// `base_url`. Loaded by the route from the tenant root.
    adapters: Option<cosmon_core::config::AdaptersConfig>,
    /// The uid that will read the credential. The adapter does not
    /// demote, so the worker runs as the adapter process — but the uid is
    /// carried explicitly rather than read at check time so the whole
    /// predicate is testable.
    worker_uid: u32,
    /// The credentials file this deployment declares, when the adapter
    /// carries a Claude login surface.
    ///
    /// This is the SAME path `GET /v1/auth/me` classifies
    /// ([`crate::routes::auth_me`]) and the same one the PKCE confirm
    /// handler writes. Sharing it is what makes the two answers cohere by
    /// construction rather than by two checks that merely happen to
    /// agree: `claude_credentials_present: false` and a refused tackle are
    /// now the same verdict on the same artifact, read through the same
    /// classifier. Issue #48 shipped both halves; the v3.10 bake found
    /// only the tackle half missing.
    ///
    /// `None` — no login surface configured — falls back to the ambient
    /// worker-environment resolution, which is the developer-machine
    /// deployment where the operator's own `claude` login is the
    /// credential.
    credentials_path: Option<PathBuf>,
}

impl RppSpawnPreflight {
    /// Build the preconditions for one dispatch.
    ///
    /// `credentials_path` is the deployment's declared credentials file
    /// (`AppState::auth_claude`), or `None` when no login surface is
    /// configured.
    #[must_use]
    pub fn new(
        envelope: WorkerEnvelope,
        tenant_root: &Path,
        credentials_path: Option<PathBuf>,
    ) -> Self {
        let config_path = cosmon_runtime::TenantPaths::rooted_at(tenant_root).config_path;
        let adapters = cosmon_filestore::load_project_config(&config_path)
            .ok()
            .and_then(|cfg| cfg.adapters);
        Self {
            envelope,
            adapters,
            worker_uid: nix::unistd::Uid::effective().as_raw(),
            credentials_path,
        }
    }

    /// Compile the environment the spawned worker will actually read.
    ///
    /// This is the envelope's own `build_env` over this process's
    /// environment — the same call [`crate::worker_env::EnvelopedBackend`]
    /// makes at spawn time. Asking the credential question against any
    /// other snapshot is how a check passes a dispatch whose worker cannot
    /// start.
    fn worker_env(&self) -> Vec<(String, String)> {
        self.envelope.build_env(std::env::vars())
    }

    /// Resolve the backend a `local`-family adapter will dial, when this
    /// tenant declared one.
    ///
    /// Config only, and `None` when the tenant configured nothing — see
    /// the module docs on why the CLI's env chain is deliberately not
    /// reproduced here, and [`SpawnPreflight::check`] on why an
    /// unconfigured tenant is not probed at the server's own loopback.
    fn local_base_url(&self, adapter: &str) -> Option<String> {
        self.adapters
            .as_ref()
            .and_then(|a| a.entries.get(adapter))
            .and_then(|entry| entry.base_url.clone())
    }
}

/// Decide whether the credentials file this deployment declares would let
/// a worker start — the same question, on the same artifact, through the
/// same classifier `GET /v1/auth/me` answers.
///
/// # Why the shared classifier, and not a second opinion
///
/// Issue #48 replaced auth/me's `path.exists()` with
/// [`classify_credentials_file`], because a file holding `{}` or a token
/// that expired months ago reported `present: true` while the worker
/// refused to start — a signal that confirmed the wrong hypothesis at the
/// exact moment someone was hunting the real cause. A tackle precondition
/// that re-derived the verdict independently would re-open the same gap
/// from the other side: auth/me could say "absent" while tackle spawned,
/// or the reverse. One classifier, one artifact, one verdict.
///
/// [`CredentialsVerdict::Refreshable`] passes: the worker's own `claude`
/// refreshes an expired token from the refresh token, and refusing it
/// here would refuse a dispatch that works.
///
/// # Errors
///
/// A [`PreflightRefusal::WorkerCredentialMissing`] naming the verdict.
fn check_declared_credentials(adapter: &str, path: &Path) -> Result<(), PreflightRefusal> {
    let verdict = classify_credentials_file(path);
    if verdict.is_usable() {
        return Ok(());
    }
    Err(PreflightRefusal::WorkerCredentialMissing {
        adapter: adapter.to_owned(),
        detail: format!(
            "the credentials file this deployment declares is {} ({})",
            verdict.as_wire(),
            path.display()
        ),
        remedy: concat!(
            "Complete the Claude login on this instance (`cs-remote login claude`, or the ",
            "operator flow that writes the credentials file), then retry the dispatch. ",
            "`GET /v1/auth/me` reports the same verdict under `claude_credentials_status`."
        )
        .to_owned(),
    })
}

/// Decide whether a Claude worker spawned under `worker_env` as
/// `worker_uid` would find a credential it can use.
///
/// Split out of [`RppSpawnPreflight`] as a free function over its two
/// inputs so the predicate is testable without a tokio runtime, a tenant
/// root, or a mutation of this process's environment.
///
/// # Errors
///
/// A [`PreflightRefusal::WorkerCredentialMissing`] naming the failed
/// precondition and its repair.
fn check_worker_credential(
    adapter: &str,
    worker_env: &[(String, String)],
    worker_uid: u32,
) -> Result<(), PreflightRefusal> {
    let lookup = |key: &str| {
        worker_env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    // `config_dir` is read from the worker's env, not this process's:
    // `CLAUDE_CONFIG_DIR` is absent from the allow-list, so this is
    // `None` in every shipped deployment — which is exactly the lookup
    // the worker performs, and the one that derives the right keychain
    // service name.
    let config_dir = lookup("CLAUDE_CONFIG_DIR");
    cosmon_transport::claude_login::check_tui_credentials(
        config_dir.as_deref(),
        worker_uid,
        lookup,
        cosmon_transport::claude_login::security_keychain_probe,
    )
    .map(|_source| ())
    .map_err(|refusal| PreflightRefusal::WorkerCredentialMissing {
        adapter: adapter.to_owned(),
        detail: refusal.to_string(),
        remedy: refusal.remedy(),
    })
}

/// Ollama's OpenAI-compatible `/v1/models` envelope.
///
/// `data` is optional: a freshly-installed daemon with nothing pulled
/// answers `{"object":"list","data":null}`, and a non-optional field would
/// make that a *parse* error — misreporting the empty-daemon case as an
/// unreachable one and naming the wrong repair.
#[derive(serde::Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Option<Vec<ModelEntry>>,
}

/// One entry of [`ModelsResponse`].
#[derive(serde::Deserialize)]
struct ModelEntry {
    /// The model identifier the worker would ask for.
    id: String,
}

/// Probe a local OpenAI-compatible backend for reachability and, when a
/// model is pinned, for the ability to serve it.
///
/// Probes `/v1/models` rather than a native admin surface because `/v1`
/// is the surface the worker itself dials — proving the endpoint the work
/// will use, not a neighbouring one.
///
/// # Errors
///
/// A [`PreflightRefusal::AdapterBackendUnreachable`] whose `detail` names
/// which of the two failures occurred, so the server log states the
/// repair (start the daemon vs. pull the model).
async fn probe_local_backend(
    adapter: &str,
    base_url: &str,
    model: Option<&str>,
) -> Result<(), PreflightRefusal> {
    let unreachable = |detail: String| PreflightRefusal::AdapterBackendUnreachable {
        adapter: adapter.to_owned(),
        detail,
    };
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| unreachable(format!("http client build failed: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| unreachable(format!("backend at {base_url} did not answer: {e}")))?;
    if !resp.status().is_success() {
        return Err(unreachable(format!(
            "backend at {base_url} answered HTTP {}",
            resp.status()
        )));
    }
    let parsed: ModelsResponse = resp.json().await.map_err(|e| {
        unreachable(format!(
            "unreadable /v1/models response from {base_url}: {e}"
        ))
    })?;
    let Some(model) = model else {
        // No pin: reachability is the whole precondition.
        return Ok(());
    };
    let available: Vec<String> = parsed
        .data
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.id)
        .collect();
    if available.iter().any(|m| m == model) {
        return Ok(());
    }
    let served = if available.is_empty() {
        "it serves no models at all — none have been pulled".to_owned()
    } else {
        format!("it serves: {}", available.join(", "))
    };
    Err(unreachable(format!(
        "backend at {base_url} cannot serve model '{model}' — {served}"
    )))
}

impl SpawnPreflight for RppSpawnPreflight {
    fn check(&self, ctx: &PreflightContext<'_>) -> Result<(), PreflightRefusal> {
        if CLAUDE_ADAPTERS.contains(&ctx.adapter) {
            return match self.credentials_path.as_deref() {
                Some(path) => check_declared_credentials(ctx.adapter, path),
                None => check_worker_credential(ctx.adapter, &self.worker_env(), self.worker_uid),
            };
        }
        if LOCAL_ADAPTERS.contains(&ctx.adapter) {
            // Only a tenant that DECLARED a backend is probed. A `local`
            // adapter with no `base_url` is not a claim that a daemon
            // listens on the adapter process's loopback — it is the
            // absence of a claim, and probing `localhost` on the server's
            // behalf would make one tenant's dispatch depend on what
            // happens to run next to the server. `cs tackle` probes its
            // default because there the loopback IS the operator's
            // machine; here it is not.
            let Some(base_url) = self.local_base_url(ctx.adapter) else {
                return Ok(());
            };
            // A dedicated current-thread runtime rather than the ambient
            // handle: this runs on the blocking pool the tackle route
            // dispatches on, and `Handle::block_on` of the server's own
            // current-thread runtime from another thread is not a thing
            // that can be relied on. Building one costs a few
            // microseconds against a probe already budgeted 3 s.
            let probe = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PreflightRefusal::AdapterBackendUnreachable {
                    adapter: ctx.adapter.to_owned(),
                    detail: format!("could not build the probe runtime: {e}"),
                })?;
            return probe.block_on(probe_local_backend(ctx.adapter, &base_url, ctx.model));
        }
        // Adapters with no stated precondition (the Direct-API arms) pass.
        // Silence here is a deliberate "no precondition is known", not a
        // claim that the dispatch will work.
        Ok(())
    }
}

/// Absolute path of the credentials file the *worker* would read.
///
/// Exposed so `GET /v1/auth/me` and this preflight answer the
/// worker-glasses question about the **same** artifact — see
/// [`crate::routes::auth_me`]. Returns `None` when neither
/// `CLAUDE_CONFIG_DIR` nor `HOME` reaches the worker, which is itself the
/// `UnknownConfigHome` refusal.
#[must_use]
pub fn worker_credentials_path(worker_env: &[(String, String)]) -> Option<PathBuf> {
    let lookup = |key: &str| {
        worker_env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let config_dir = lookup("CLAUDE_CONFIG_DIR");
    cosmon_transport::claude_login::credentials_file(config_dir.as_deref(), lookup).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// The regression the v3.10 bake measured, as a unit test: a worker
    /// environment with a `HOME` whose `.claude` holds no credential is
    /// REFUSED, and the refusal carries the issue-#48 label.
    #[test]
    fn empty_config_home_refuses_with_worker_credential_missing() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".claude")).expect("mkdir");
        let refusal = check_worker_credential(
            "claude",
            &env(&[("HOME", &home.path().to_string_lossy())]),
            nix::unistd::Uid::effective().as_raw(),
        )
        .expect_err("a config home with no credential must refuse");
        assert_eq!(refusal.label(), "worker_credential_missing");
    }

    /// A provisioned credential file passes — the check is a precondition,
    /// not a blanket refusal of every containerised dispatch.
    #[test]
    fn provisioned_credentials_file_passes() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join(".claude");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"t","refreshToken":"r","expiresAt":9999999999999,"scopes":[]}}"#,
        )
        .expect("write");
        check_worker_credential(
            "claude",
            &env(&[("HOME", &home.path().to_string_lossy())]),
            nix::unistd::Uid::effective().as_raw(),
        )
        .expect("a provisioned credential must pass");
    }

    /// The adapter's OWN `CLAUDE_CODE_OAUTH_TOKEN` must not green-light a
    /// dispatch: the allow-list does not carry it, so the worker will
    /// never see it. This is the false-green that mirrors the false 200.
    #[test]
    fn token_absent_from_the_worker_env_does_not_satisfy_the_check() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".claude")).expect("mkdir");
        // The token is deliberately NOT in the worker env passed below,
        // exactly as `PASSTHROUGH_VARS` guarantees at spawn time.
        assert!(
            !crate::worker_env::PASSTHROUGH_VARS.contains(&"CLAUDE_CODE_OAUTH_TOKEN"),
            "if the allow-list ever carries the token, this check must read it too"
        );
        let refusal = check_worker_credential(
            "claude",
            &env(&[("HOME", &home.path().to_string_lossy())]),
            nix::unistd::Uid::effective().as_raw(),
        )
        .expect_err("a token the worker cannot see must not satisfy the precondition");
        assert_eq!(refusal.label(), "worker_credential_missing");
    }

    /// An unreachable backend refuses with the second stable label.
    #[tokio::test]
    async fn unreachable_backend_refuses_with_adapter_backend_unreachable() {
        // Port 1 on loopback: reserved, never bound by a test fixture.
        let refusal = probe_local_backend("local", "http://127.0.0.1:1", Some("qwen3:8b"))
            .await
            .expect_err("an unreachable backend must refuse");
        assert_eq!(refusal.label(), "adapter_backend_unreachable");
    }
}
