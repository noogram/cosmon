// SPDX-License-Identifier: AGPL-3.0-only

//! Second-family review of PR #57, finding 2 — **the adapter's worker
//! spawns must land on the tenant's project tmux socket**, never on a
//! shared literal.
//!
//! Architectural invariant §7f: *the tmux socket name is derived from the
//! project and never shared across projects*, with
//! [`cosmon_filestore::resolve_tmux_socket_name`] as the single source of
//! truth — the same resolver `cs tackle` (`cmd::tmux_socket_name`) and
//! `cs done` route through. Before this fix the adapter built one
//! `TmuxBackend::new("cosmon")` for every tenant, so two tenant galaxies
//! shared a tmux namespace and a CLI `cs done` looked for the worker on a
//! socket the adapter had never used.
//!
//! RED before the fix: the adapter exposes no per-tenant resolution at all.

use cosmon_rpp_adapter::worker_env::WorkerBackends;

/// Materialize a tenant galaxy: a `.cosmon/config.toml` carrying an
/// explicit `project_id`, which is what a provisioned noyau looks like.
fn tenant(root: &std::path::Path, project_id: &str) {
    let cosmon = root.join(".cosmon");
    std::fs::create_dir_all(&cosmon).expect("tenant .cosmon");
    std::fs::write(
        cosmon.join("config.toml"),
        format!("[project]\nproject_id = \"{project_id}\"\n"),
    )
    .expect("tenant config");
}

#[test]
fn each_tenant_spawns_on_its_own_project_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    tenant(&alpha, "alpha-1a2b");
    tenant(&beta, "beta-3c4d");

    let alpha_socket = WorkerBackends::socket_for(&alpha);
    let beta_socket = WorkerBackends::socket_for(&beta);

    // The same resolution the CLI performs — not a parallel scheme.
    assert_eq!(
        alpha_socket,
        cosmon_filestore::resolve_tmux_socket_name(&alpha.join(".cosmon").join("config.toml")),
        "the adapter must route through `resolve_tmux_socket_name` (§7f)"
    );
    assert_eq!(alpha_socket, "alpha-1a2b");
    assert_eq!(beta_socket, "beta-3c4d");
    assert_ne!(
        alpha_socket, beta_socket,
        "two tenant galaxies must never share a tmux namespace (§7f)"
    );
    assert_ne!(
        alpha_socket, "cosmon",
        "the shared socket literal is exactly what §7f forbids"
    );
}

/// The backend handed to the executor is the tenant's, and the cache
/// returns the same socket for the same tenant rather than minting a new
/// server per dispatch.
#[test]
fn the_per_tenant_backend_carries_that_tenants_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alpha = dir.path().join("alpha");
    tenant(&alpha, "alpha-1a2b");

    let backends = WorkerBackends::per_project_tmux();
    assert_eq!(backends.socket_of(&alpha), Some("alpha-1a2b".to_owned()));
    assert_eq!(
        backends.socket_of(&alpha),
        Some("alpha-1a2b".to_owned()),
        "a repeated dispatch must reuse the tenant's backend"
    );
}
