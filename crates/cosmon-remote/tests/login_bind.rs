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

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cosmon-remote")
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
