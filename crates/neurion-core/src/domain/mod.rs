// SPDX-License-Identifier: Apache-2.0

pub mod drift;
pub mod galaxy_kind;
pub mod reachable;
pub mod registry;

pub use drift::{
    editorial_to_introspection_drift, hub_to_project_drift, infra_to_imposition_drift,
    project_to_frozen_drift, vanity_family_drift, DriftReport, DriftVerdict, FrozenObservation,
    HubWindow, VanityObservation,
};
pub use galaxy_kind::{classify_known_galaxy, GalaxyKind};
#[allow(unused_imports)] // Used when OxyMake/Cosmon integration lands
pub use reachable::Reachable;
pub use registry::{
    compute_health, default_score, intent_score, Category, GraphEndpoint, HealthStatus, Intent,
    InventorySearchResult, PersonSurface, RankedReach, ReachProfile, RegistryPort,
};
