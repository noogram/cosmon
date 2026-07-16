// SPDX-License-Identifier: Apache-2.0

//! The Reachable trait -- the shared abstraction across Neurion, OxyMake, and Cosmon.
//!
//! A logical entity can be materialized through multiple physical carriers.
//! The optimal carrier depends on the consumer's intent and the carrier's properties.
//! This is the "synaptic selection" principle from the THESIS.

/// The core abstraction: multi-materialization with intent-driven selection.
///
/// Three systems implement this pattern:
/// - **Neurion**: Referent=knowledge domain, Bearer=access endpoint, Intent=Read/Write/Search/Verify
/// - **OxyMake**: Referent=OutputRef, Bearer=Materialization(mem/disk/store), Intent=() (always cheapest)
/// - **Cosmon**: Referent=Message, Bearer=Channel(IPC/JSONL/SQLite), Intent=Critical/Audit/Bulk
#[allow(dead_code)] // Shared abstraction — implemented when OxyMake/Cosmon integrate
pub trait Reachable {
    /// The logical entity (knowledge domain, job output, message).
    type Referent;
    /// The physical carrier (access endpoint, materialization, channel).
    type Bearer;
    /// What the consumer needs to do (read/write/search, cheapest read, critical/audit).
    type Intent;
    /// Comparable cost metric.
    type Cost: PartialOrd;

    /// All carriers for a given referent, with their costs.
    fn reaches(&self, referent: &Self::Referent) -> Vec<(Self::Bearer, Self::Cost)>;

    /// Select the optimal carrier for a given referent and intent.
    fn select(&self, referent: &Self::Referent, intent: &Self::Intent) -> Option<Self::Bearer>;
}
