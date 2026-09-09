// SPDX-License-Identifier: AGPL-3.0-only

//! `login --bind` — the CLI half of the configurable callback bind address
//! (GitHub issue #52).
//!
//! The flag moves the *listener* the OAuth redirect catcher opens; it never
//! moves the advertised `redirect_uri`, which stays the `127.0.0.1` literal
//! registered with the provider. These tests pin the surface a tenant actually
//! types: the flag exists, it is typed (an `IpAddr`, refused before any I/O),
//! and its refusal is loud rather than silent.

use std::process::Command;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cosmon-remote")
}

/// Where `ProfileStore::default_location` resolves `<config_dir>/cosmon-remote`
/// for a given `$HOME` — reproduced here (rather than pulled in from `dirs`) so
/// the test plants the profile at exactly the path a `$HOME`-scoped child
/// process will look it up under, on the two platforms this suite runs on.
fn config_root_under(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/cosmon-remote")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".config/cosmon-remote")
    }
}

/// Mount a minimal OIDC discovery pair (client registry + provider metadata) on
/// a loopback `wiremock` server, so `login`'s `oidc::discover` round-trip stays
/// on-host — the test stays hermetic (no network beyond loopback).
async fn mount_discovery(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 2,
            "issuer": server.uri(),
            "clients": [{"audience": "cs-rpp-adapter", "client_id": "client-A"}],
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(server)
        .await;
}

/// Plant a ready-to-resolve profile named `test` under a scratch `$HOME`,
/// pointed at `server` for both `host` and `oidc_url` (mirrors the
/// `oidc_flow.rs` unit-test shape).
fn write_profile(home: &std::path::Path, server: &MockServer) {
    let root = config_root_under(home);
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("config.toml"), "default_profile = \"test\"\n").unwrap();
    std::fs::write(
        root.join("profiles").join("test.toml"),
        format!(
            "host = {:?}\nsub = \"operator\"\naud = \"cs-rpp-adapter\"\noidc_url = {:?}\n",
            server.uri(),
            server.uri()
        ),
    )
    .unwrap();
}

/// Collapse the help text's line wrapping, which depends on terminal width, so
/// an assertion is about the words and not about where clap broke them.
fn unwrapped(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A value that is not an IP address must be refused by the argument parser —
/// non-zero exit, a message naming the offending value — *before* any listener
/// is opened or any network call made.
#[test]
fn an_invalid_bind_value_is_refused_before_anything_binds() {
    let out = Command::new(bin())
        .args(["login", "--bind", "not-an-ip"])
        .output()
        .expect("run the tenant binary");

    assert!(
        !out.status.success(),
        "an unparseable --bind must fail; got {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not-an-ip"),
        "the refusal must name the offending value, got: {stderr}"
    );
    // The failure is a *parse* refusal, not a downstream one: nothing was bound
    // and nothing was dialled on the way to it.
    assert!(
        !stderr.contains("could not bind"),
        "no listener may be opened on a rejected --bind, got: {stderr}"
    );
}

/// A `host:port` value is refused too: the port is not this flag's business. It
/// stays the redirect port, so the listener and the advertised URI can never
/// disagree about it.
#[test]
fn a_host_port_value_is_refused_the_flag_carries_the_address_only() {
    let out = Command::new(bin())
        .args(["login", "--bind", "0.0.0.0:9999"])
        .output()
        .expect("run the tenant binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("0.0.0.0:9999"), "got: {stderr}");
}

/// The flag is documented where a tenant looks for it, and the help states the
/// invariant that makes it safe to offer: the advertised redirect URI does not
/// move with the listener.
#[test]
fn login_help_documents_the_flag_and_the_unchanged_redirect_uri() {
    let out = Command::new(bin())
        .args(["login", "--help"])
        .output()
        .expect("run the tenant binary");
    assert!(out.status.success());
    let help = unwrapped(&String::from_utf8_lossy(&out.stdout));
    assert!(help.contains("--bind <IP>"), "got: {help}");
    assert!(help.contains("default 127.0.0.1"), "got: {help}");
    assert!(
        help.contains("advertised redirect URI never changes"),
        "the help must state that the advertised URI is unchanged, got: {help}"
    );
}

/// Wiring-level regression: the non-loopback stderr notice in `run_login`
/// (`src/main.rs`, `if !endpoints.bind().is_loopback()`) has no coverage that
/// exercises the real binary — deleting the call site left the suite green.
/// This spawns `cosmon-remote login --bind <non-loopback>` for real, with
/// discovery served from a loopback mock so the run stays hermetic, and
/// asserts the notice lands on stderr. `203.0.113.7` (TEST-NET-3, RFC 5737)
/// is never a local interface, so the callback-server bind that follows the
/// notice fails immediately with an OS error — no browser opens, no timeout is
/// waited out, and the login is expected to fail *after* the notice was
/// printed. RED with the call site removed, GREEN with it restored (recorded
/// in the molecule's result.md).
#[tokio::test]
async fn a_non_loopback_bind_prints_the_notice_before_the_bind_itself_fails() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;

    let home = tempfile::TempDir::new().unwrap();
    write_profile(home.path(), &server);

    let out = Command::new(bin())
        .args(["login", "--bind", "203.0.113.7"])
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env("COSMON_REMOTE_CRED_BACKEND", "file")
        .output()
        .expect("run the tenant binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("203.0.113.7:"),
        "the non-loopback bind must be named on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("not loopback"),
        "the notice must state the exposure, got: {stderr}"
    );
    // The listener bind on a non-local address fails, so login cannot
    // complete — but that failure is expected, and irrelevant to this test:
    // it proves only that the process ran past discovery and the notice.
    assert!(
        !out.status.success(),
        "a bind on a non-local address must not succeed"
    );
}
