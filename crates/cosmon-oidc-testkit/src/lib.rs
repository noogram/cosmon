// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-oidc-testkit` — fixtures for the §8j HTTPS+OIDC ingress
//! adapters defined in [ADR-080].
//!
//! Two primitives ship together because they exercise opposite ends of
//! the same admission boundary:
//!
//! - [`OidcMock`] — a self-contained `IdP`: an in-memory JWKS endpoint
//!   (axum + tokio), an embedded RSA-2048 signing key, and a
//!   [`OidcMock::issue_jwt`] helper that produces tokens compatible
//!   with `cosmon-rpp-adapter::JwtVerifier`.
//! - [`tenant_workspace`] — a `TempDir` factory that lays out a per-noyau
//!   `~/galaxies/<noyau>/.cosmon/state/` tree, the canonical subprocess
//!   `cwd` from ADR-080 §3.5 clause (e). Multi-tenant variants live on
//!   [`TenantWorkspaces`].
//!
//! The crate is **dev-only**. Adapters depend on it via
//! `[dev-dependencies]`; nothing from this crate ever links into a
//! production binary.
//!
//! # Why this exists
//!
//! The tenant-isolation guarantee turns on a single
//! invariant: a JWT scoped to `noyau=A` cannot, through any path
//! exposed by the RPP, read state owned by `noyau=B`. The structural
//! defence is the per-tenant subprocess `cwd` (clause (e)) — a `cs`
//! invocation in `~/galaxies/A/` cannot resolve a molecule that lives
//! under `~/galaxies/B/.cosmon/state/`. This crate makes that test
//! cheap to write and impossible to forget.
//!
//! [ADR-080]: ../../docs/adr/080-remote-pilot-port-https-oidc.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Pedantic noise that fires on idiomatic test-fixture code:
// `from_static` panics, fixed-size buffer arithmetic, etc.
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod mock;
mod workspace;

pub use mock::{
    fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, DEFAULT_AUDIENCE, DEFAULT_ISSUER, DEFAULT_KID,
};
pub use workspace::{tenant_workspace, TenantPath, TenantWorkspace, TenantWorkspaces};

/// Public RSA test key (PEM). Embedded so consumers can install it as
/// a JWKS file in the format `cosmon-rpp-adapter::JwksStore::load`
/// expects, without depending on any private fixture path.
pub const TEST_RSA_PUBLIC_PEM: &str = include_str!("../assets/test_rsa_public.pem");

/// Private RSA test key (PEM). Used internally to sign issued JWTs.
/// Exported so consumers can replicate the issuance side-channel in
/// non-tokio test scaffolds.
///
/// **DEMO ONLY.** This key is committed in plaintext to the repository
/// and MUST NEVER appear in a production credential store.
pub const TEST_RSA_PRIVATE_PEM: &str = include_str!("../assets/test_rsa_private.pem");

/// Base64URL-encoded RSA modulus (`n`) of [`TEST_RSA_PUBLIC_PEM`].
/// Pre-computed so the JWKS builder needs no ASN.1 dependency.
pub const TEST_RSA_N_B64URL: &str = "xxASTBLueNPmGuyEnI4dU2Cpv8QpMxBNoEUy1X09lboYCY35Ra1cfh9OYV3nm06BJdzmRP9AijW0rfR9Bxuo2MioLwLo40ITBnwzzVGv1kCUxJFwnEKPsOJn1g4_ZCG_U98ayhyCRBnghqwzr8V2l2rnAVeVZXGzbP0rGNVoPV2XGErKhnfg9Yc_choFqr2hXvbwp2XaiCPgMtCg5AC9vfDzbtH_84rlNcLZLdM8rLWIBf9DmJ4uGF6YQgdx1JWJ-qjMQg94k1cqQ2vJZYOCxiQhFDOVRAFhhyxkG0XHsg2Ylo9CD57IYstG0adGGQ8toqgMmpG_UprnU_zT-liLQw";

/// Base64URL-encoded RSA public exponent (`e`) of
/// [`TEST_RSA_PUBLIC_PEM`]. The standard 65537 (`AQAB`).
pub const TEST_RSA_E_B64URL: &str = "AQAB";

/// Canonical numeric uid **and** gid the container user runs as — in
/// both the `cs-oidc-mock` image (`crates/cosmon-oidc-testkit/Dockerfile`)
/// and the `cosmon-rpp-adapter` image.
///
/// The two services mount the shared `rpp-jwks` named volume; Docker
/// stamps files with the *numeric* id, not the user name, so the two
/// images MUST agree bit-for-bit or the JWKS hand-off silently
/// `EACCES`-es. This const is the **single source of truth**. Each
/// `Dockerfile` restates the number as `ARG COSMON_UID=<n>` — Docker
/// cannot read a Rust const at build time, so this is a
/// cross-toolchain copy of the same value. We cannot delete
/// that copy; instead the snapshot trace in
/// `tests/uid_seed_snapshot.rs` pins every `Dockerfile`'s `ARG`
/// default to this const. Edit this one number and the trace tells you
/// which `Dockerfile` drifted.
pub const COSMON_CONTAINER_UID: u32 = 10000;
