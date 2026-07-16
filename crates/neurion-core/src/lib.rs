// SPDX-License-Identifier: Apache-2.0

//! neurion-core: Pure domain types for the nervous system.
//!
//! Referents, reaches, the Reachable trait, and scoring functions.
//! Zero I/O — all external interactions go through neurion-mcp.

pub mod auto_register;
pub mod domain;
pub mod schema;

// Re-exports for convenience.
pub use domain::drift::{
    editorial_to_introspection_drift, hub_to_project_drift, infra_to_imposition_drift,
    project_to_frozen_drift, vanity_family_drift, DriftReport, DriftVerdict, FrozenObservation,
    HubWindow, VanityObservation,
};
pub use domain::galaxy_kind::{classify_known_galaxy, GalaxyKind};
pub use domain::reachable::Reachable;
pub use domain::registry::{
    Category, GraphEndpoint, HealthStatus, Intent, InventorySearchResult, PersonSurface,
    RankedReach, ReachProfile, RegistryPort,
};
