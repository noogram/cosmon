// SPDX-License-Identifier: AGPL-3.0-only

//! The briefing delivery postcondition (issue #40), re-exported.
//!
//! The implementation lives in [`cosmon_transport::briefing_delivery`] so the
//! RPP API's in-process executor holds a briefing to the same postcondition
//! as `cs tackle` (issue #81). This module keeps the historical path.

pub use cosmon_transport::briefing_delivery::*;
