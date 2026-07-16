// SPDX-License-Identifier: Apache-2.0

//! Deterministic bind-address resolution for HTTP-on-Tailscale daemons.
//!
//! Three policies, evaluated in order of explicitness:
//!
//! 1. [`TailscaleBind::Explicit`] — caller passes a `SocketAddr` directly.
//! 2. [`TailscaleBind::EnvOrAuto`] — read the `COCKPIT_HTTP_BIND` env var
//!    if set; otherwise auto-discover via `tailscale ip --4`.
//! 3. [`TailscaleBind::Auto`] — auto-discover unconditionally.
//!
//! In every path the resolved address is rejected if it is unspecified
//! (`0.0.0.0` / `::`). The whole point of this crate is to keep the
//! daemon inside the Tailscale trust boundary; binding to all interfaces
//! is a security regression and we surface it as an error rather than a
//! warning.

use std::net::{IpAddr, SocketAddr};
use std::process::Command;

use thiserror::Error;

/// How to resolve the address an axum router should listen on.
#[derive(Debug, Clone)]
pub enum TailscaleBind {
    /// Caller-supplied address. Validated for non-unspecified IP.
    Explicit(SocketAddr),
    /// Read `COCKPIT_HTTP_BIND` from the environment if present, else
    /// auto-discover the Tailscale IPv4 and bind on `port`.
    EnvOrAuto { port: u16, env_var: &'static str },
    /// Always auto-discover the Tailscale IPv4, bind on `port`.
    Auto { port: u16 },
}

impl TailscaleBind {
    /// Convenience: auto-discover and bind on `port`. Equivalent to
    /// `TailscaleBind::EnvOrAuto { port, env_var: "COCKPIT_HTTP_BIND" }`.
    #[must_use]
    pub fn auto_with_port(port: u16) -> Self {
        Self::EnvOrAuto {
            port,
            env_var: "COCKPIT_HTTP_BIND",
        }
    }
}

/// Where the resolved address came from. Used by the access log to make
/// the binding policy debuggable in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindSource {
    Explicit,
    Env,
    Auto,
}

impl BindSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Env => "env",
            Self::Auto => "auto",
        }
    }
}

/// Successful resolution: the address to bind, where it came from, and
/// the optional Tailscale hostname (for diagnostic logging).
#[derive(Debug, Clone)]
pub struct BindOutcome {
    pub addr: SocketAddr,
    pub source: BindSource,
    pub hostname: Option<String>,
}

#[derive(Debug, Error)]
pub enum BindError {
    #[error("refusing to bind on unspecified address {0} — apps-transport-http requires a Tailscale-scoped IP (see docs/runbook/http-on-tailscale.md)")]
    UnspecifiedAddress(SocketAddr),
    #[error("failed to invoke `tailscale ip --4`: {0}")]
    TailscaleIpInvoke(std::io::Error),
    #[error("`tailscale ip --4` exited with status {0}; output: {1}")]
    TailscaleIpStatus(i32, String),
    #[error("`tailscale ip --4` returned no IPv4 address")]
    TailscaleIpEmpty,
    #[error("invalid bind address `{0}`: {1}")]
    InvalidAddress(String, String),
}

/// Resolve a [`TailscaleBind`] into a concrete [`BindOutcome`].
///
/// Pure function over policy + environment; no networking is performed.
///
/// # Errors
///
/// Returns [`BindError::UnspecifiedAddress`] if resolution yields a
/// `0.0.0.0` / `::` address, [`BindError::InvalidAddress`] for a
/// malformed env override, [`BindError::TailscaleIpInvoke`] /
/// [`BindError::TailscaleIpStatus`] / [`BindError::TailscaleIpEmpty`]
/// when the auto-discover path fails.
pub fn resolve_bind(bind: &TailscaleBind) -> Result<BindOutcome, BindError> {
    let (addr, source) = match bind {
        TailscaleBind::Explicit(a) => (*a, BindSource::Explicit),
        TailscaleBind::EnvOrAuto { port, env_var } => match std::env::var(env_var) {
            Ok(v) if !v.trim().is_empty() => (parse_socket_addr(&v)?, BindSource::Env),
            _ => (
                SocketAddr::new(discover_tailscale_ip()?, *port),
                BindSource::Auto,
            ),
        },
        TailscaleBind::Auto { port } => (
            SocketAddr::new(discover_tailscale_ip()?, *port),
            BindSource::Auto,
        ),
    };
    if addr.ip().is_unspecified() {
        return Err(BindError::UnspecifiedAddress(addr));
    }
    let hostname = std::env::var("COCKPIT_HTTP_HOSTNAME")
        .ok()
        .filter(|s| !s.trim().is_empty());
    Ok(BindOutcome {
        addr,
        source,
        hostname,
    })
}

fn parse_socket_addr(s: &str) -> Result<SocketAddr, BindError> {
    s.parse::<SocketAddr>()
        .map_err(|e| BindError::InvalidAddress(s.to_string(), e.to_string()))
}

fn discover_tailscale_ip() -> Result<IpAddr, BindError> {
    if let Ok(v) = std::env::var("APPS_TRANSPORT_HTTP_TAILSCALE_IP") {
        if !v.trim().is_empty() {
            return v
                .trim()
                .parse::<IpAddr>()
                .map_err(|e| BindError::InvalidAddress(v.clone(), e.to_string()));
        }
    }
    let output = Command::new("tailscale")
        .args(["ip", "--4"])
        .output()
        .map_err(BindError::TailscaleIpInvoke)?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(BindError::TailscaleIpStatus(code, stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or(BindError::TailscaleIpEmpty)?;
    line.parse::<IpAddr>()
        .map_err(|e| BindError::InvalidAddress(line.to_string(), e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn explicit_loopback_resolves() {
        let out = resolve_bind(&TailscaleBind::Explicit(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            8789,
        )))
        .unwrap();
        assert_eq!(out.source, BindSource::Explicit);
        assert_eq!(out.addr.port(), 8789);
    }

    #[test]
    fn explicit_unspecified_is_rejected() {
        let err = resolve_bind(&TailscaleBind::Explicit(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            8789,
        )))
        .unwrap_err();
        assert!(matches!(err, BindError::UnspecifiedAddress(_)));
    }

    #[test]
    fn env_or_auto_uses_env_when_set() {
        let env_var = "APPS_TRANSPORT_HTTP_TEST_ENV_BIND_ENV";
        // Set to a valid loopback so we don't depend on `tailscale` in CI.
        std::env::set_var(env_var, "127.0.0.1:8123");
        let out = resolve_bind(&TailscaleBind::EnvOrAuto {
            port: 9999,
            env_var,
        })
        .unwrap();
        std::env::remove_var(env_var);
        assert_eq!(out.source, BindSource::Env);
        assert_eq!(out.addr.port(), 8123);
    }

    #[test]
    fn env_or_auto_rejects_invalid_env() {
        let env_var = "APPS_TRANSPORT_HTTP_TEST_ENV_BIND_BAD";
        std::env::set_var(env_var, "not-an-addr");
        let err = resolve_bind(&TailscaleBind::EnvOrAuto {
            port: 9999,
            env_var,
        })
        .unwrap_err();
        std::env::remove_var(env_var);
        assert!(matches!(err, BindError::InvalidAddress(_, _)));
    }

    #[test]
    fn auto_uses_override_env_when_present() {
        std::env::set_var("APPS_TRANSPORT_HTTP_TAILSCALE_IP", "192.0.2.10");
        let out = resolve_bind(&TailscaleBind::Auto { port: 8789 }).unwrap();
        std::env::remove_var("APPS_TRANSPORT_HTTP_TAILSCALE_IP");
        assert_eq!(out.source, BindSource::Auto);
        assert_eq!(out.addr.ip().to_string(), "192.0.2.10");
        assert_eq!(out.addr.port(), 8789);
    }
}
