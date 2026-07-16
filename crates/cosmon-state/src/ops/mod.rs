// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon_state::ops` — library-first verbs over the state store.
//!
//! Each verb is a pure function that takes a [`crate::StateStore`]
//! reference plus the addressing parameters it needs, and returns a
//! verb-specific view + a verb-specific error. This is the library-first
//! pattern: cs-cli and cs-api call the same function so we stop shelling
//! out for read-only routes.
//!
//! Today the namespace contains the V0 read-only + V1 mutation cuts
//! exposed over the §8j RPP boundary:
//! [`observe`], [`tag`], [`nucleate`],
//! [`ensemble`] / [`collapse`] / [`freeze`] / [`thaw`] / [`stuck`].
//!
//! Every verb-level error implements [`error::OpsError`] — the wire
//! contract that lets the cs-cli, the cs-api, and the out-of-process
//! Receptionist (RPP) map a failure to a stable kebab-case tag and an
//! HTTP status without `match`-on-string. See [`error`] for the trait
//! and the [`error::ErrorWire`] payload that crosses process boundaries.
//!
//! Callers that need the byte-for-byte JSON wire format consumed by
//! external scripts (`cs --json observe`) should use the renderer
//! helpers exposed alongside the function (e.g. [`observe::ObserveJson`])
//! rather than serializing the view directly.

pub mod await_operator;
pub mod collapse;
pub mod ensemble;
pub mod error;
pub mod freeze;
pub mod model_attribution;
pub mod nucleate;
pub mod observe;
pub mod stuck;
pub mod tag;
pub mod thaw;

pub use await_operator::{
    await_operator, AwaitOperatorError, AwaitOperatorJson, AwaitOperatorOutcome,
    AwaitOperatorRequest, AwaitOperatorView,
};
pub use collapse::{collapse, CollapseError, CollapseJson, CollapseRequest, CollapseView};
pub use ensemble::{
    ensemble, EnsembleEntryJson, EnsembleError, EnsembleJson, EnsembleRequest, EnsembleView,
};
pub use error::{is_kebab_case, ErrorWire, OpsError};
pub use freeze::{freeze, FreezeError, FreezeJson, FreezeRequest, FreezeView};
pub use model_attribution::{latest_model_selection, model_selections, ModelAttribution};
pub use nucleate::{nucleate, NucleateError, NucleateJson, NucleateRequest, NucleateView};
pub use observe::{
    detect_ghost, observe, observe_loaded, MoleculeView, ObserveError, ObserveJson,
    ObserveResponse, ResponseMetrics,
};
pub use stuck::{stuck, StuckError, StuckJson, StuckRequest, StuckView};
pub use tag::{tag, TagDelta, TagError, TagJson};
pub use thaw::{thaw, ThawError, ThawJson, ThawRequest, ThawView};
