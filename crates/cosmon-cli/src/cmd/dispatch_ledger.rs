// SPDX-License-Identifier: AGPL-3.0-only

//! Re-export shim — the dispatch ledger lives in
//! [`cosmon_runtime::dispatch_ledger`] since issue #54 / U5.
//!
//! The ledger was born binary-private here, which made "record a dispatch"
//! reachable only by shelling the `cs` binary — the exact runtime
//! dependency issue #54 retires. The module moved wholesale (types, ordering
//! contract, tests) to the runtime crate, the library home of dispatch
//! execution; this shim keeps every historical `crate::cmd::dispatch_ledger`
//! path compiling so the move is invisible to the CLI's call sites.

pub use cosmon_runtime::dispatch_ledger::*;
