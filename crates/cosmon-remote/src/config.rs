// SPDX-License-Identifier: AGPL-3.0-only

//! Per-deployment configuration.
//!
//! Two-file layout under `$XDG_CONFIG_HOME/cosmon-remote/` (defaults to
//! `~/.config/cosmon-remote/` via [`dirs::config_dir`]):
//!
//! - `config.toml`        — top-level (only `default_profile = "<name>"`).
//! - `profiles/<name>.toml` — one file per deployment. Holds the full
//!   four-tuple `(host, sub, aud, oidc_url)` plus the optional `noyau`,
//!   the JWT scopes the operator is allowed to mint, and any
//!   per-deployment knobs (timeouts, artifact_dir).
//!
//! Why two files: the operator switches deployments by editing one tiny
//! file (`default_profile = "tenant-demo-aws"`) without touching the per-host
//! configuration — same gesture as `aws --profile` or `kubectl context`.
//! It also lets `install.sh` (Phase 1 dist) drop a new profile in place
//! without merging into a single growing file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Environment variable name overriding the active profile for a single
/// invocation. The `--profile` flag takes precedence.
pub const ENV_PROFILE: &str = "COSMON_REMOTE_PROFILE";

/// Environment variable carrying the JWT to present to the adapter. When
/// set, the CLI skips the OIDC mint step entirely — used by CI and by
/// the smoke-test harness.
pub const ENV_TOKEN: &str = "COSMON_REMOTE_TOKEN";

/// Top-level config file (lives at `<dir>/config.toml`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TopConfig {
    /// Profile name resolved when neither `--profile` nor `$COSMON_REMOTE_PROFILE` is set.
    pub default_profile: Option<String>,
    /// Whether the operator has acknowledged the credit guard shown
    /// by `do` before its first worker dispatch (« this launches an
    /// agent and burns credit »). `None`/`false` → the guard prompts;
    /// a confirmed interactive *yes* persists `true` so the question
    /// is asked once — the credit guard is shown to the operator before
    /// any spend on their wallet. Additive + skipped when
    /// absent: existing config.toml files round-trip byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_guard_acknowledged: Option<bool>,
}

/// One deployment profile. Lives at `<dir>/profiles/<name>.toml`.
///
/// The four-tuple `(host, sub, aud, oidc_url)` is **not** templated by
/// the binary at runtime — it is read verbatim and used as-is. The
/// AWS live-deploy test showed that brittle "host pinned, sub/aud/oidc_url
/// guessed" templating in install.sh / justfile produced `mol-list →
/// null` with no useful diagnostic. The Phase 1 contract is that every
/// field is set explicitly, either by the operator (`config set`) or by
/// `install.sh` at the server (templated server-side at fetch time).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    /// Base URL of the cosmon-rpp-adapter — e.g. `https://cosmon.example.ts.net`.
    /// The scheme is preserved as-is (learning #3 from the live deployment test).
    pub host: String,
    /// JWT `sub` claim used when minting OIDC tokens (e.g. `tenant-demo-operator`).
    pub sub: String,
    /// JWT `aud` claim (e.g. `cosmon-rpp-tenant`).
    pub aud: String,
    /// OIDC mock issuer URL — endpoint that mints JWTs for this deployment.
    /// Typically `<host>/oidc` but explicitly templated per deployment
    /// (some Tailscale-served instances expose the mock on a different host).
    pub oidc_url: String,
    /// The discovered OIDC `issuer` (`iss`) of the real Forgejo IdP, recorded by
    /// `login` (delib-20260710-33b7 C2/C8). Together with [`Self::client_id`]
    /// this lets every subsequent command rebuild the credential key
    /// `(issuer, sub, aud=client_id)` and reach the persisted token **offline**,
    /// so the silent-refresh fast path needs no discovery round-trip. Absent on
    /// mock deployments (which mint via `oidc_url/issue`); additive and skipped
    /// when unset so pre-existing profiles round-trip byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// The provisioned OAuth `client_id` (`== aud`) learned via reverse-discovery
    /// and recorded by `login` (delib-20260710-33b7 C8). A **changed** value on a
    /// later login is a re-provision — the old credential slot is orphaned and a
    /// fresh login writes the new one. Public (not a secret); additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Optional noyau name (multi-noyau deployments). The adapter
    /// resolves the noyau from the JWT `sub`; this field is a label for
    /// the operator and may be echoed back in display.
    #[serde(default)]
    pub noyau: Option<String>,
    /// Default scopes the operator mints. Per-call scopes can override.
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// Where artefacts fetched via `artifact get` are written. Default
    /// is `./cosmon-artifacts/`.
    #[serde(default)]
    pub artifacts_dir: Option<PathBuf>,
    /// HTTP timeout in seconds for normal calls. Default 30.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Passive opt-out remontée. When a
    /// failure predicts abandonment (503, 502, 4xx burst), the CLI
    /// queues `request_id + error code` — never artifact content,
    /// never the sub — and the pair rides the next successful request.
    /// The client cuts with one gesture: `config set phone-home off`.
    ///
    /// Serialised only when it deviates from the default (`true`) so
    /// pre-existing tenant profiles stay byte-identical: the file
    /// records the opt-out *gesture*, not the default state.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub phone_home: bool,
}

fn default_scopes() -> Vec<String> {
    vec![
        "cosmon:molecule:read".into(),
        "cosmon:molecule:write".into(),
    ]
}

const fn default_timeout() -> u64 {
    30
}

const fn default_true() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_true(b: &bool) -> bool {
    *b
}

impl Profile {
    /// Build a fresh profile from just a host URL. The other fields are
    /// filled with empty placeholders the operator MUST set before any
    /// API call works — the binary does NOT guess defaults that
    /// silently pass JWT validation against the wrong audience (root
    /// cause of the AWS live-deploy `mol-list → null`).
    pub fn from_host(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            sub: String::new(),
            aud: String::new(),
            oidc_url: String::new(),
            issuer: None,
            client_id: None,
            noyau: None,
            scopes: default_scopes(),
            artifacts_dir: None,
            timeout_secs: default_timeout(),
            phone_home: true,
        }
    }

    /// Report any field the operator MUST set before API calls work.
    /// `auth login` and every molecule/artifact verb call this and abort
    /// with a precise message instead of producing a null response.
    pub fn check_ready(&self) -> Result<()> {
        let missing: Vec<&'static str> = [
            ("host", self.host.is_empty()),
            ("sub", self.sub.is_empty()),
            ("aud", self.aud.is_empty()),
            ("oidc_url", self.oidc_url.is_empty()),
        ]
        .into_iter()
        .filter_map(|(name, missing)| if missing { Some(name) } else { None })
        .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Error::Config(format!(
                "profile is missing required fields: {}. Run `cosmon-remote config set <key> <value>` for each, or rerun `install.sh` against the host so the server templates them.",
                missing.join(", ")
            )))
        }
    }

    /// Whether this profile targets the **real** Forgejo OIDC flow (a `login`
    /// has recorded both the discovered `issuer` and the provisioned
    /// `client_id`), as opposed to a mock deployment that mints via
    /// `oidc_url/issue`. The token-resolution path (`client_for`) reads the
    /// persisted credential + silent refresh only for real-OIDC profiles; mock
    /// profiles keep the legacy mint behaviour untouched.
    pub fn is_real_oidc(&self) -> bool {
        self.issuer.as_deref().is_some_and(|s| !s.is_empty())
            && self.client_id.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// The credential-key `aud` component: the provisioned `client_id` when a
    /// real login recorded one, else the logical `aud` (the mock label). Named
    /// in the deliberation synthesis (`effective_client_id` falling back to
    /// `aud`).
    pub fn effective_client_id(&self) -> &str {
        self.client_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.aud)
    }

    /// Apply a `key=value` pair from `config set <key> <value>`. The
    /// accepted keys are intentionally enumerated — adding a knob is a
    /// new code path, not a free-form patch surface.
    pub fn set(&mut self, key: &str, value: String) -> Result<()> {
        match key {
            "host" => self.host = value,
            "sub" => self.sub = value,
            "aud" => self.aud = value,
            "oidc-url" | "oidc_url" => self.oidc_url = value,
            "issuer" => self.issuer = if value.is_empty() { None } else { Some(value) },
            "client-id" | "client_id" => {
                self.client_id = if value.is_empty() { None } else { Some(value) };
            }
            "noyau" => self.noyau = if value.is_empty() { None } else { Some(value) },
            "timeout" | "timeout-secs" | "timeout_secs" => {
                self.timeout_secs = value
                    .parse()
                    .map_err(|e| Error::Config(format!("timeout must be an integer: {e}")))?;
            }
            "artifacts-dir" | "artifacts_dir" => {
                self.artifacts_dir = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                };
            }
            "phone-home" | "phone_home" => {
                self.phone_home = match value.to_ascii_lowercase().as_str() {
                    "off" | "false" | "0" | "no" => false,
                    "on" | "true" | "1" | "yes" => true,
                    other => {
                        return Err(Error::Config(format!(
                            "phone-home must be on|off, got {other:?}"
                        )));
                    }
                };
            }
            other => {
                return Err(Error::Config(format!(
                    "unknown config key {other:?}; accepted: host, sub, aud, oidc-url, issuer, client-id, noyau, timeout, artifacts-dir, phone-home"
                )));
            }
        }
        Ok(())
    }
}

/// The on-disk profile store. Cheap to construct; reads from disk lazily.
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    /// Standard location: `<config_dir>/cosmon-remote/`.
    pub fn default_location() -> Result<Self> {
        let base = dirs::config_dir()
            .ok_or_else(|| Error::Config("could not resolve $XDG_CONFIG_HOME".into()))?;
        Ok(Self::at(base.join("cosmon-remote")))
    }

    /// Build a store rooted at an explicit path (used by tests).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn top_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn profile_path(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(format!("{name}.toml"))
    }

    /// Read the top-level config, returning [`TopConfig::default`] when absent.
    pub fn read_top(&self) -> Result<TopConfig> {
        let path = self.top_path();
        if !path.exists() {
            return Ok(TopConfig::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Write the top-level config (creating the directory tree if needed).
    pub fn write_top(&self, top: &TopConfig) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let text = toml::to_string_pretty(top)?;
        std::fs::write(self.top_path(), text)?;
        Ok(())
    }

    /// Read a profile by name.
    pub fn read_profile(&self, name: &str) -> Result<Profile> {
        let path = self.profile_path(name);
        if !path.exists() {
            return Err(Error::Config(format!(
                "profile {name:?} not found at {}",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Write a profile by name.
    pub fn write_profile(&self, name: &str, profile: &Profile) -> Result<()> {
        std::fs::create_dir_all(self.profiles_dir())?;
        let text = toml::to_string_pretty(profile)?;
        std::fs::write(self.profile_path(name), text)?;
        Ok(())
    }

    /// List every known profile name (alphabetised, deterministic).
    pub fn list_profiles(&self) -> Result<Vec<String>> {
        let dir = self.profiles_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_owned());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Resolve the active profile name. Precedence:
    /// 1. explicit `--profile` flag (caller passes `Some(name)`),
    /// 2. `$COSMON_REMOTE_PROFILE`,
    /// 3. `default_profile` in `config.toml`,
    /// 4. error.
    pub fn resolve_name(&self, flag: Option<&str>) -> Result<String> {
        if let Some(name) = flag {
            return Ok(name.to_owned());
        }
        if let Ok(name) = std::env::var(ENV_PROFILE) {
            if !name.is_empty() {
                return Ok(name);
            }
        }
        let top = self.read_top()?;
        top.default_profile.ok_or_else(|| {
            Error::Config(
                "no profile selected. Pass --profile <name>, set $COSMON_REMOTE_PROFILE, or run `cosmon-remote config use <name>`.".into()
            )
        })
    }

    /// Resolve the active profile (name + parsed `Profile`).
    pub fn resolve(&self, flag: Option<&str>) -> Result<(String, Profile)> {
        let name = self.resolve_name(flag)?;
        let profile = self.read_profile(&name)?;
        Ok((name, profile))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(tmp: &TempDir) -> ProfileStore {
        ProfileStore::at(tmp.path().join("cosmon-remote"))
    }

    #[test]
    fn init_sets_only_host() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let p = Profile::from_host("https://example.invalid");
        s.write_profile("demo", &p).unwrap();
        let read = s.read_profile("demo").unwrap();
        assert_eq!(read.host, "https://example.invalid");
        assert!(read.sub.is_empty());
        assert!(read.check_ready().is_err());
    }

    #[test]
    fn set_fields_then_ready() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let mut p = Profile::from_host("http://127.0.0.1:8443");
        p.set("sub", "tenant-demo-operator".into()).unwrap();
        p.set("aud", "cosmon-rpp-tenant-demo".into()).unwrap();
        p.set("oidc-url", "http://127.0.0.1:8444".into()).unwrap();
        p.set("noyau", "default".into()).unwrap();
        s.write_profile("tenant-demo", &p).unwrap();
        let read = s.read_profile("tenant-demo").unwrap();
        read.check_ready().unwrap();
        assert_eq!(read.noyau.as_deref(), Some("default"));
    }

    #[test]
    fn phone_home_default_on_opt_out_persists_and_default_is_unwritten() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        // Default: enabled, and NOT serialised (frozen-config gate).
        let p = Profile::from_host("http://x");
        assert!(p.phone_home);
        s.write_profile("demo", &p).unwrap();
        let text = std::fs::read_to_string(s.profile_path("demo")).unwrap();
        assert!(!text.contains("phone_home"));
        // The gesture: off persists across a write/read cycle.
        let mut p = s.read_profile("demo").unwrap();
        p.set("phone-home", "off".into()).unwrap();
        assert!(!p.phone_home);
        s.write_profile("demo", &p).unwrap();
        let read = s.read_profile("demo").unwrap();
        assert!(!read.phone_home);
        // And back on: the key disappears again.
        let mut p = read;
        p.set("phone-home", "on".into()).unwrap();
        s.write_profile("demo", &p).unwrap();
        let text = std::fs::read_to_string(s.profile_path("demo")).unwrap();
        assert!(!text.contains("phone_home"));
        // Garbage value is refused.
        assert!(p.set("phone-home", "maybe".into()).is_err());
    }

    #[test]
    fn unknown_key_rejected() {
        let mut p = Profile::from_host("http://x");
        let err = p.set("unknown-field", "v".into()).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn list_profiles_sorted() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        s.write_profile("zeta", &Profile::from_host("http://z"))
            .unwrap();
        s.write_profile("alpha", &Profile::from_host("http://a"))
            .unwrap();
        s.write_profile("mu", &Profile::from_host("http://m"))
            .unwrap();
        let names = s.list_profiles().unwrap();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn resolve_name_precedence_flag_over_env() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        s.write_top(&TopConfig {
            default_profile: Some("default".into()),
            credit_guard_acknowledged: None,
        })
        .unwrap();
        unsafe {
            std::env::set_var(ENV_PROFILE, "from-env");
        }
        let name = s.resolve_name(Some("from-flag")).unwrap();
        assert_eq!(name, "from-flag");
        let name = s.resolve_name(None).unwrap();
        assert_eq!(name, "from-env");
        unsafe {
            std::env::remove_var(ENV_PROFILE);
        }
        let name = s.resolve_name(None).unwrap();
        assert_eq!(name, "default");
    }

    #[test]
    fn missing_profile_yields_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let err = s.read_profile("ghost").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ghost"),
            "expected profile name in error: {msg}"
        );
    }
}
