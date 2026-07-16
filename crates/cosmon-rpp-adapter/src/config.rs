// SPDX-License-Identifier: AGPL-3.0-only

//! Operator configuration — `~/.config/cosmon/rpp.toml`.
//!
//! Layout:
//!
//! ```toml
//! # Where the adapter binds (defaults to 127.0.0.1:8443).
//! bind_addr = "0.0.0.0:8443"
//!
//! # Posture: "prepared" (V0 default, warns) or "active".
//! posture = "prepared"
//!
//! # Optional explicit path to the `cs` binary; falls back to PATH.
//! cs_path = "/usr/local/bin/cs"
//!
//! # Cosmon state directory. Overridable by `COSMON_STATE_DIR` in the
//! # environment (env > rpp.toml > default) so a per-deployment compose
//! # that mounts its persistent volume elsewhere wins over this baked
//! # value — see [`RppConfig::resolved_state_dir`].
//! state_dir = "~/.cosmon/state"
//!
//! # Where the API audit envelopes land
//! # (defaults to <state_dir parent>/whispers/inbox).
//! whispers_inbox_root = "~/.cosmon/whispers/inbox"
//!
//! # Galaxy root for tenant routing.
//! galaxies_root = "~/galaxies"
//!
//! # Per-subprocess timeout in seconds.
//! subprocess_timeout_sec = 30
//!
//! # JWKS HTTP-fetch refresh interval, seconds (default 3600 = 1 h).
//! # The trusted-issuer allowlist itself lives in
//! # <state_dir>/security/trusted-issuers.toml (see jwks_fetch).
//! jwks_refresh_ttl_sec = 3600
//!
//! # Model pin for tenant claude worker sessions (avatar-surface D1).
//! # Absent → [`DEFAULT_CLAUDE_MODEL`]; "" → opt out (claude's own
//! # default); any other value → exported verbatim.
//! claude_model = "<model-id>"
//! ```
//!
//! All paths support `~` expansion against `$HOME` at load time.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Posture;

/// Default bind address for the adapter.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8443";

/// On-disk shape of the operator config.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RppConfig {
    /// Listening address (defaults to [`DEFAULT_BIND_ADDR`]).
    pub bind_addr: Option<String>,
    /// Posture switch — `prepared` (default) or `active`.
    pub posture: Option<Posture>,
    /// Override the `cs` binary path.
    pub cs_path: Option<PathBuf>,
    /// Override the cosmon state directory.
    pub state_dir: Option<PathBuf>,
    /// Override the whisper inbox root.
    pub whispers_inbox_root: Option<PathBuf>,
    /// Override the galaxies root.
    pub galaxies_root: Option<PathBuf>,
    /// Subprocess timeout, seconds.
    pub subprocess_timeout_sec: Option<u64>,
    /// JWKS HTTP-fetch refresh interval, seconds. Default
    /// [`crate::jwks_fetch::DEFAULT_REFRESH_TTL`] (1 h). The TTL is only
    /// the background net; urgent rotation is covered instantly by the
    /// on-demand cache-miss, so the default is deliberately coarse
    /// (smithy spec §2.3). Tunable per deployment without touching the
    /// allowlist.
    pub jwks_refresh_ttl_sec: Option<u64>,
    /// LLM backends declared by the operator. Listed so the
    /// `/health/backends` diagnostic surface (T-V1-IFBDD-METER)
    /// reports them as `configured-but-unused` until the wrapping
    /// `LlmBackend::complete` records its first probe. Stringly-typed
    /// by design — the IFBDD goal is to learn which names show up in
    /// practice before crystallising a typed enum. Defaults to the
    /// empty list, in which case the endpoint reports an empty
    /// `backends` array until probes land.
    #[serde(default)]
    pub backends: Vec<String>,
    /// Root directory for per-molecule artifact dirs (e653 spec).
    /// Default
    /// [`crate::routes::artifacts::DEFAULT_ARTIFACT_ROOT`] (i.e.
    /// `/tmp/cosmon`). Override to relocate artifacts off `/tmp`
    /// (e.g. onto a persistent volume).
    pub artifact_root: Option<PathBuf>,
    /// Root directory under which the four per-platform
    /// `cosmon-remote` binaries live, served by
    /// `GET /dist/binary/{platform}/cosmon-remote` (Phase 1 dist
    /// multi-OS). Default
    /// [`crate::routes::dist::DEFAULT_DIST_ROOT`] (i.e.
    /// `/opt/cosmon-remote/dist`, the path every shipping Dockerfile
    /// COPYs the binaries to). The default matches the image layout, so
    /// this override is only needed on a
    /// non-Docker host to repoint at a checkout's
    /// `crates/cosmon-rpp-adapter/assets/binaries/` — it is no longer
    /// load-bearing for the baked images.
    pub dist_root: Option<PathBuf>,
    /// Model pin for the `claude` sessions of tenant molecules
    /// (avatar-surface D1). The pin lives
    /// HERE — operator binding, readable by the tenant, never written
    /// by it — and nowhere else: not in code, not in a formula, not in
    /// the client CLAUDE.md (the copy nobody re-syncs at
    /// the next model). Read at worker-spawn time by the §3.5
    /// subprocess envelope ([`crate::subprocess::SystemInvoker`]) and
    /// exported as `ANTHROPIC_MODEL` into the `cs tackle` child, which
    /// threads it across the tmux boundary into the worker `claude`
    /// command. Absent → [`DEFAULT_CLAUDE_MODEL`]. Explicit `""` →
    /// opt-out (no export; the claude CLI uses its own default).
    /// Changing the fleet's model is a one-line diff of this key.
    pub claude_model: Option<String>,
    /// Per-deployment values the adapter substitutes into the served
    /// `install.sh` (Phase 1 templating). When
    /// set, the operator's `install.sh | sh` lands the
    /// `cosmon-remote` binary AND writes a ready-to-use profile to
    /// `~/.config/cosmon-remote/profiles/<host>.toml` — resolving the
    /// "AWS live-deploy test, `COSMON_HOST` seulement" finding by
    /// persisting the full four-tuple instead of just the host.
    /// Missing fields are emitted as empty strings; install.sh skips
    /// the corresponding `config set` line rather than writing a
    /// literal placeholder.
    #[serde(default)]
    pub install_templating: InstallTemplating,
    /// Boot-time trust bootstrap (ADR-141): where the server looks for
    /// `IdP` handoff files and which issuers are statically declared.
    /// Absent → no handoff ingestion, no static declaration; only the
    /// `TRUSTED_*` env trio (if set) is converged.
    #[serde(default)]
    pub trust_bootstrap: crate::trust_bootstrap::TrustBootstrapSection,
}

/// Default model pin for tenant claude worker sessions when the
/// operator leaves `claude_model` unset (avatar-surface D1; the
/// latest Anthropic model).
///
/// This is the **head** of the model fallback chain — the preferred
/// model. The model-id literal itself now lives in **one** place,
/// [`cosmon_core::model_chain::DEFAULT_MODEL_CHAIN`]
/// ([`PREFERRED_MODEL`](cosmon_core::model_chain::PREFERRED_MODEL) is its
/// first entry); this constant re-exports it so the adapter default and
/// the `cs tackle` probe order can never drift out of sync at the next
/// model change (the `aec8` class of bug: the copy nobody re-syncs).
/// Moving the fleet is a one-line diff of that slice — or, per
/// deployment, of the `claude_model` key in `rpp.toml`, which wins over
/// this default and is hoisted to the head of the chain.
///
/// The pin is no longer a hard floor: when
/// the preferred model is unreachable the `cs tackle` spawn path probes
/// the rest of the chain (`claude-opus-4-8` → `claude-sonnet-4-6`)
/// rather than spawning a worker that would freeze on `model_not_found`.
pub const DEFAULT_CLAUDE_MODEL: &str = cosmon_core::model_chain::PREFERRED_MODEL;

/// Default `oidc_url` when the operator leaves it unset
/// (Pierre hardening P3). Every cosmon-server
/// deployment serves its own OIDC surface at `<host>/oidc`, so the
/// adapter templates that path by default rather than emitting the
/// "no per-deployment fields configured server-side" comment and
/// forcing the tenant to discover the URL by hand. The
/// `__COSMON_HOST__` placeholder is substituted with the request base
/// URL at fetch time ([`crate::routes::serve_install_sh`]), so a
/// single default tracks whichever host the `install.sh` was fetched
/// from. An operator who fronts OIDC on a *separate* host overrides
/// this with the absolute URL; an operator who wants no `oidc-url`
/// line at all sets it explicitly to the empty string (opt-out).
pub const DEFAULT_OIDC_URL_TEMPLATE: &str = "__COSMON_HOST__/oidc";

/// Per-deployment values templated into the served `install.sh` so
/// the tenant lands with a ready-to-use profile. The fields mirror
/// the four-tuple [`crate::routes::dist`] expects (see Phase 1
/// architectural finding "host seulement, sub/aud/oidc-url devinés
/// par templating brittle" from the AWS live-deploy test).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct InstallTemplating {
    /// JWT `sub` claim. e.g. `tenant-demo-operator`.
    pub sub: String,
    /// JWT `aud` claim. e.g. `cosmon-rpp-tenant`.
    pub aud: String,
    /// OIDC issuer URL minting JWTs for this deployment. Supports
    /// the literal `__COSMON_HOST__` placeholder which is replaced
    /// at fetch time with the request base URL — convenient when
    /// the OIDC mock is served at a path on the same host (e.g.
    /// `__COSMON_HOST__/oidc`). For separate-host setups, store
    /// the absolute URL. Defaults to
    /// [`DEFAULT_OIDC_URL_TEMPLATE`] (`__COSMON_HOST__/oidc`) so the
    /// served `install.sh` always persists an `oidc-url` even when the
    /// operator configures nothing (Pierre hardening P3); set to `""`
    /// to opt out.
    pub oidc_url: String,
    /// Optional noyau label. e.g. `tenant-demo`. The adapter resolves
    /// the actual noyau from the JWT `sub`; this field is a display
    /// label in the operator's profile.
    pub noyau: String,
}

impl Default for InstallTemplating {
    fn default() -> Self {
        Self {
            sub: String::new(),
            aud: String::new(),
            oidc_url: DEFAULT_OIDC_URL_TEMPLATE.to_owned(),
            noyau: String::new(),
        }
    }
}

impl RppConfig {
    /// Load and tilde-expand the config at `path`. Missing files
    /// resolve to `RppConfig::default()` so the adapter can boot
    /// from environment variables alone.
    ///
    /// # Errors
    ///
    /// Returns the parse error if the TOML is malformed.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let mut cfg: Self = toml::from_str(&text).map_err(ConfigError::Toml)?;
        cfg.expand_paths();
        Ok(cfg)
    }

    /// Expand every path-typed field against `$HOME`.
    fn expand_paths(&mut self) {
        self.cs_path = self.cs_path.take().map(expand_tilde);
        self.state_dir = self.state_dir.take().map(expand_tilde);
        self.whispers_inbox_root = self.whispers_inbox_root.take().map(expand_tilde);
        self.galaxies_root = self.galaxies_root.take().map(expand_tilde);
        self.artifact_root = self.artifact_root.take().map(expand_tilde);
        self.dist_root = self.dist_root.take().map(expand_tilde);
    }

    /// Resolved bind address (with default fallback).
    #[must_use]
    pub fn resolved_bind_addr(&self) -> String {
        self.bind_addr
            .clone()
            .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned())
    }

    /// Resolved cosmon state directory.
    ///
    /// Precedence — **env > rpp.toml > default**:
    ///
    /// 1. `COSMON_STATE_DIR` when set and non-empty. The per-deployment
    ///    compose contract (env + the volume mounted at that path) wins
    ///    over a value baked into the image's `rpp.toml`. This closes the
    ///    `state_dir` divergence class: an image whose baked
    ///    `state_dir = "/cosmon/.cosmon/state"` is deployed against a
    ///    compose that mounts its persistent volume at
    ///    `/var/lib/cosmon-state` (and exports `COSMON_STATE_DIR` to
    ///    match) no longer silently writes its state — trusted-issuers,
    ///    nucleon bindings, galaxies — off the mounted volume onto an
    ///    ephemeral (or read-only) rootfs path. The precedence mirrors
    ///    `cosmon-filestore`'s resolver (env above walk-up) and the
    ///    `tackle-503` fix that re-poses `COSMON_STATE_DIR` for the `cs`
    ///    subprocess.
    /// 2. The `state_dir` key from `rpp.toml`.
    /// 3. `$HOME/.cosmon/state` ([`default_state_dir`]).
    ///
    /// The helper is passive: when `COSMON_STATE_DIR` is unset (or empty)
    /// the resolved path is byte-identical to the legacy `rpp.toml`/default
    /// behaviour, so nothing changes for deployments that align the two.
    #[must_use]
    pub fn resolved_state_dir(&self) -> PathBuf {
        self.resolved_state_dir_from(env_state_dir())
    }

    /// Env-free core of [`resolved_state_dir`] — takes the
    /// `COSMON_STATE_DIR` override explicitly so the precedence is unit
    /// testable without mutating this process's environment (which would
    /// race parallel tests).
    #[must_use]
    fn resolved_state_dir_from(&self, env_override: Option<PathBuf>) -> PathBuf {
        env_override
            .or_else(|| self.state_dir.clone())
            .unwrap_or_else(default_state_dir)
    }

    /// Which layer supplied the resolved [`resolved_state_dir`], as a
    /// static label for the `boot.paths` diagnostic. The `state_dir`
    /// divergence was originally diagnosed by reading that log line and
    /// noticing it disagreed with the compose env; surfacing the winning
    /// source makes the mismatch self-evident without a code trace.
    #[must_use]
    pub fn state_dir_source(&self) -> &'static str {
        if env_state_dir().is_some() {
            "env:COSMON_STATE_DIR"
        } else if self.state_dir.is_some() {
            "rpp.toml"
        } else {
            "default"
        }
    }

    /// Resolved whisper inbox root (sibling of `state/`).
    #[must_use]
    pub fn resolved_inbox_root(&self) -> PathBuf {
        self.whispers_inbox_root.clone().unwrap_or_else(|| {
            let state_dir = self.resolved_state_dir();
            let base = state_dir
                .parent()
                .map_or_else(|| state_dir.clone(), Path::to_path_buf);
            base.join("whispers").join("inbox")
        })
    }

    /// Resolved galaxies root.
    #[must_use]
    pub fn resolved_galaxies_root(&self) -> PathBuf {
        self.galaxies_root
            .clone()
            .unwrap_or_else(default_galaxies_root)
    }

    /// Resolved posture (default `Prepared`).
    #[must_use]
    pub fn resolved_posture(&self) -> Posture {
        self.posture.unwrap_or_default()
    }

    /// Resolved subprocess timeout (default
    /// [`crate::DEFAULT_SUBPROCESS_TIMEOUT`]).
    #[must_use]
    pub fn resolved_subprocess_timeout(&self) -> std::time::Duration {
        self.subprocess_timeout_sec.map_or(
            crate::DEFAULT_SUBPROCESS_TIMEOUT,
            std::time::Duration::from_secs,
        )
    }

    /// Resolved backend list — empty by default.
    #[must_use]
    pub fn resolved_backends(&self) -> Vec<String> {
        self.backends.clone()
    }

    /// Resolved JWKS refresh interval (default
    /// [`crate::jwks_fetch::DEFAULT_REFRESH_TTL`]).
    #[must_use]
    pub fn resolved_jwks_refresh_ttl(&self) -> std::time::Duration {
        self.jwks_refresh_ttl_sec.map_or(
            crate::jwks_fetch::DEFAULT_REFRESH_TTL,
            std::time::Duration::from_secs,
        )
    }

    /// Resolved artifact root (e653 spec).
    /// Default
    /// [`crate::routes::artifacts::DEFAULT_ARTIFACT_ROOT`].
    #[must_use]
    pub fn resolved_artifact_root(&self) -> PathBuf {
        self.artifact_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(crate::routes::artifacts::DEFAULT_ARTIFACT_ROOT))
    }

    /// Resolved model pin for tenant claude worker sessions
    /// (avatar-surface D1). Three-state resolution:
    ///
    /// - key absent → `Some(`[`DEFAULT_CLAUDE_MODEL`]`)` — the pin
    ///   applies by default;
    /// - `claude_model = ""` → `None` — explicit opt-out, no env
    ///   export, the claude CLI falls back to its own default;
    /// - any other value → `Some(value)` — operator override,
    ///   exported verbatim.
    #[must_use]
    pub fn resolved_claude_model(&self) -> Option<String> {
        match &self.claude_model {
            None => Some(DEFAULT_CLAUDE_MODEL.to_owned()),
            Some(s) if s.is_empty() => None,
            Some(s) => Some(s.clone()),
        }
    }

    /// Resolved dist root (Phase 1 dist multi-OS). Default
    /// [`crate::routes::dist::DEFAULT_DIST_ROOT`].
    #[must_use]
    pub fn resolved_dist_root(&self) -> PathBuf {
        self.dist_root
            .clone()
            .unwrap_or_else(|| PathBuf::from(crate::routes::dist::DEFAULT_DIST_ROOT))
    }
}

fn expand_tilde(p: PathBuf) -> PathBuf {
    if let Ok(s) = p.clone().into_os_string().into_string() {
        if let Some(rest) = s.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
    }
    p
}

fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".cosmon/state")
}

/// The `COSMON_STATE_DIR` override, as a path — `None` when the variable
/// is unset or empty. An empty value is treated as absent so a compose
/// `COSMON_STATE_DIR=` (or `${VAR:-}` expanding to nothing) falls through
/// to `rpp.toml` rather than resolving the state root to the process CWD.
fn env_state_dir() -> Option<PathBuf> {
    std::env::var_os("COSMON_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn default_galaxies_root() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("galaxies")
}

/// Errors raised while loading the config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem read error.
    #[error("read config: {0}")]
    Io(std::io::Error),
    /// TOML deserialisation error.
    #[error("parse config: {0}")]
    Toml(toml::de::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_resolve_to_sensible_values() {
        let cfg = RppConfig::default();
        assert_eq!(cfg.resolved_bind_addr(), DEFAULT_BIND_ADDR);
        assert_eq!(cfg.resolved_posture(), Posture::Prepared);
    }

    #[test]
    fn missing_file_returns_default() {
        let cfg = RppConfig::load(std::path::Path::new("/nonexistent")).unwrap();
        assert_eq!(cfg.resolved_bind_addr(), DEFAULT_BIND_ADDR);
    }

    #[test]
    fn parses_active_posture() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "posture = \"active\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert_eq!(cfg.resolved_posture(), Posture::Active);
    }

    // ── state_dir divergence — env > rpp.toml > default (task-874a) ──────

    #[test]
    fn state_dir_env_overrides_baked_rpp_toml() {
        // The parc failure mode: rpp.toml bakes one path, the compose
        // mounts the persistent volume at another and exports
        // COSMON_STATE_DIR to match. The env must win so state lands on
        // the mounted volume, not the baked (ephemeral/RO) path.
        let mut cfg = RppConfig::default();
        cfg.state_dir = Some(PathBuf::from("/cosmon/.cosmon/state"));
        let resolved = cfg.resolved_state_dir_from(Some(PathBuf::from("/var/lib/cosmon-state")));
        assert_eq!(resolved, PathBuf::from("/var/lib/cosmon-state"));
    }

    #[test]
    fn state_dir_falls_back_to_rpp_toml_when_env_absent() {
        // Passive helper: no COSMON_STATE_DIR → byte-identical to the
        // legacy rpp.toml behaviour (aligned deployments are unaffected).
        let mut cfg = RppConfig::default();
        cfg.state_dir = Some(PathBuf::from("/cosmon/.cosmon/state"));
        let resolved = cfg.resolved_state_dir_from(None);
        assert_eq!(resolved, PathBuf::from("/cosmon/.cosmon/state"));
    }

    #[test]
    fn state_dir_falls_back_to_default_when_env_and_toml_absent() {
        let cfg = RppConfig::default();
        assert_eq!(cfg.resolved_state_dir_from(None), default_state_dir());
    }

    // ── avatar-surface D1 — claude_model pin resolution ──────────────────

    #[test]
    fn claude_model_absent_resolves_to_named_default() {
        // No config file at all → the pin applies. The default is the
        // single named constant; no other layer holds a model-id.
        let cfg = RppConfig::default();
        assert_eq!(
            cfg.resolved_claude_model().as_deref(),
            Some(DEFAULT_CLAUDE_MODEL)
        );
    }

    #[test]
    fn claude_model_explicit_value_overrides_default() {
        // Operator override: one config line, exported verbatim.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "claude_model = \"some-future-model\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert_eq!(
            cfg.resolved_claude_model().as_deref(),
            Some("some-future-model")
        );
    }

    #[test]
    fn claude_model_explicit_empty_opts_out() {
        // `claude_model = ""` → no pin: the env var is not exported
        // and the claude CLI uses its own default.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "claude_model = \"\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert_eq!(cfg.resolved_claude_model(), None);
    }

    // ── Pierre hardening P3 — oidc_url default templating ────────────────

    #[test]
    fn install_templating_defaults_oidc_url_to_host_template() {
        // No config file at all → the four-tuple still carries the default
        // `oidc-url` so the served install.sh persists it (wart fix).
        let cfg = RppConfig::default();
        assert_eq!(cfg.install_templating.oidc_url, DEFAULT_OIDC_URL_TEMPLATE);
        assert!(cfg.install_templating.sub.is_empty());
        assert!(cfg.install_templating.aud.is_empty());
        assert!(cfg.install_templating.noyau.is_empty());
    }

    #[test]
    fn install_templating_partial_section_keeps_default_oidc_url() {
        // An operator who sets only sub/aud (the common case) still gets the
        // default oidc-url — the missing field is filled from the struct
        // Default, not left empty.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "[install_templating]").unwrap();
        writeln!(f, "sub = \"tenant-op\"").unwrap();
        writeln!(f, "aud = \"cosmon-rpp-tenant\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert_eq!(cfg.install_templating.sub, "tenant-op");
        assert_eq!(cfg.install_templating.oidc_url, DEFAULT_OIDC_URL_TEMPLATE);
    }

    #[test]
    fn install_templating_explicit_empty_oidc_url_opts_out() {
        // The opt-out path: an operator who explicitly sets oidc_url = ""
        // suppresses the line (build_config_set_block skips empties).
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "[install_templating]").unwrap();
        writeln!(f, "oidc_url = \"\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert!(cfg.install_templating.oidc_url.is_empty());
    }

    #[test]
    fn install_templating_explicit_oidc_url_overrides_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("rpp.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "[install_templating]").unwrap();
        writeln!(f, "oidc_url = \"https://idp.example.com/oidc\"").unwrap();
        let cfg = RppConfig::load(&p).unwrap();
        assert_eq!(
            cfg.install_templating.oidc_url,
            "https://idp.example.com/oidc"
        );
    }
}
