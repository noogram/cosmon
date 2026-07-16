// SPDX-License-Identifier: AGPL-3.0-only

//! Snapshot trace pinning the container uid across the cargo↔docker
//! boundary (the uid-seed facet).
//!
//! [`cosmon_oidc_testkit::COSMON_CONTAINER_UID`] is the single source of
//! truth for the numeric uid/gid the container user runs as. Each
//! `Dockerfile` restates it as `ARG COSMON_UID=<n>` because Docker cannot
//! read a Rust const at build time — this is the class-(b) cross-toolchain
//! copy the deliberation flagged. We cannot *delete* that copy, so we
//! **pin** it: this test extracts the `ARG COSMON_UID` default from each
//! Dockerfile and asserts it equals the const.
//!
//! It is a **trace, not a runtime checker** — `cargo test` regenerates the
//! comparison and diffs; nothing watches at runtime, no daemon, no gate.
//! Both images mount the shared `rpp-jwks` named volume and Docker stamps
//! files by numeric id, not by name, so a silent drift between the two
//! uids breaks the JWKS hand-off with an `EACCES`. This test is the
//! cheapest place to catch "someone hand-edited one Dockerfile".

use cosmon_oidc_testkit::COSMON_CONTAINER_UID;

/// The `cs-oidc-mock` image Dockerfile, embedded at compile time.
const OIDC_MOCK_DOCKERFILE: &str = include_str!("../Dockerfile");

/// The `cosmon-rpp-adapter` image Dockerfile — the *other* side of the
/// shared `rpp-jwks` volume, which must run as the same numeric uid.
const RPP_ADAPTER_DOCKERFILE: &str = include_str!("../../cosmon-rpp-adapter/Dockerfile");

/// Extract the `ARG COSMON_UID=<n>` default from a Dockerfile's text.
fn arg_cosmon_uid(dockerfile: &str, label: &str) -> u32 {
    let line = dockerfile
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ARG COSMON_UID="))
        .unwrap_or_else(|| panic!("{label}: no `ARG COSMON_UID=` line found"));
    let raw = line.trim_start_matches("ARG COSMON_UID=").trim();
    raw.parse()
        .unwrap_or_else(|e| panic!("{label}: `ARG COSMON_UID={raw}` is not a u32: {e}"))
}

#[test]
fn dockerfile_uid_matches_canonical_const() {
    assert_eq!(
        arg_cosmon_uid(OIDC_MOCK_DOCKERFILE, "cs-oidc-mock Dockerfile"),
        COSMON_CONTAINER_UID,
        "cs-oidc-mock Dockerfile's `ARG COSMON_UID` drifted from \
         cosmon_oidc_testkit::COSMON_CONTAINER_UID — sync the Dockerfile."
    );
    assert_eq!(
        arg_cosmon_uid(RPP_ADAPTER_DOCKERFILE, "cosmon-rpp-adapter Dockerfile"),
        COSMON_CONTAINER_UID,
        "cosmon-rpp-adapter Dockerfile's `ARG COSMON_UID` drifted from \
         cosmon_oidc_testkit::COSMON_CONTAINER_UID — sync the Dockerfile."
    );
}
