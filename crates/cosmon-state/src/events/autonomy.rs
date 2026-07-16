// SPDX-License-Identifier: AGPL-3.0-only

//! Stable callsites for the autonomy-guard event family.
//!
//! Two free functions emit the receipts that make *"provider = local by
//! default, autonomous"* true by construction rather than merely claimed:
//!
//! - [`emit_remote_egress_opt_in`] — stamped by `cs tackle` **before** it
//!   spawns a worker for a remote adapter the operator opted into. The egress
//!   grant and the audit record are the same atom: there is no window in
//!   which a worker reaches the network without a matching line on the wire.
//! - [`emit_local_exec_receipt`] — positive per-turn evidence that a turn was
//!   produced by local inference (the polarity-flipped witness). Forgery
//!   has no receipt.
//! - [`emit_local_fallback`] — loud audit line for a conscious escalation from
//!   the local default to a remote oracle after a *decidable* local
//!   hard-failure. Minted in the same block as
//!   [`emit_remote_egress_opt_in`] so a fallback can never be silent.
//!
//! Both are **best-effort, never silent**: a serialise or write failure is
//! logged-but-swallowed (the seal-not-lock discipline). The hot path must
//! never fail because telemetry is unhappy.

use std::path::Path;

use chrono::Utc;
use cosmon_core::egress::{LocalExecReceipt, LocalFailureCause, RemoteEndpoint};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;

use crate::event_log::{emit_one, resolve_events_log_path};

/// Emit an [`EventV2::RemoteEgressOptIn`].
///
/// Called by `cs tackle` immediately *before* spawning a worker whose adapter
/// reaches a remote oracle, when the operator opted in. Stamping before spawn
/// is the load-bearing ordering: the egress grant and the audit record are
/// minted together, so a later cutover audit can trust that *every* worker
/// without a `remote_egress_opt_in` line ran strict-local.
///
/// Best-effort: write errors are logged-but-swallowed.
pub fn emit_remote_egress_opt_in(
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter_name: &str,
    endpoint: Option<&RemoteEndpoint>,
) {
    let event = EventV2::RemoteEgressOptIn {
        mol_id: mol_id.clone(),
        adapter_name: adapter_name.to_owned(),
        endpoint_host: endpoint.map(|e| e.host.clone()),
        endpoint_port: endpoint.map(|e| e.port),
        opted_in_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::LocalExecReceipt`].
///
/// Called once per agent-loop turn by a local-inference adapter. The receipt
/// carries the three legs of local-exec proof (FFI receipt, throughput band,
/// accelerator load); the cutover audit requires *every* turn
/// of ≥20 consecutive `Completed` molecules to carry a positive receipt.
///
/// Best-effort: write errors are logged-but-swallowed.
pub fn emit_local_exec_receipt(
    state_dir: &Path,
    mol_id: &MoleculeId,
    turn: u32,
    receipt: &LocalExecReceipt,
) {
    let band = match receipt.band {
        cosmon_core::egress::ThroughputBand::Local => "local",
        cosmon_core::egress::ThroughputBand::Suspect => "suspect",
    };
    let event = EventV2::LocalExecReceipt {
        mol_id: mol_id.clone(),
        turn,
        ffi_receipt: receipt.ffi_receipt,
        throughput_tok_s: receipt.throughput_tok_s,
        throughput_band: band.to_owned(),
        accelerator_load: receipt.accelerator_load,
        observed_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::EgressUnenforceable`].
///
/// Called by `cs tackle` **before** spawning a strict-local worker on a host
/// that cannot create the egress-denied network namespace (a macOS dev host,
/// or a hardened Linux kernel with unprivileged user namespaces disabled). The
/// `deny-external` policy degrades to advisory mode; this line makes the
/// degradation loud and durable so the cutover gate refuses to flip the
/// hosted-tenant default while any spawn carries it. Fix for C1-F3
/// (task-20260712-8d2d): before it, the same host produced an opaque total
/// failure with no audit trail.
///
/// Best-effort: write errors are logged-but-swallowed.
pub fn emit_egress_unenforceable(
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter_name: &str,
    reason: &str,
) {
    let event = EventV2::EgressUnenforceable {
        mol_id: mol_id.clone(),
        adapter_name: adapter_name.to_owned(),
        reason: reason.to_owned(),
        degraded_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::LocalFallback`].
///
/// Called by `cs tackle` **in the same code block** as
/// [`emit_remote_egress_opt_in`] when the operator consciously escalates a
/// local hard-failure to a remote oracle (`--adapter <remote>
/// --fallback-from-local <cause>`). Minting the two events together is the
/// load-bearing ordering: a remote call carrying a fallback cause can never
/// reach the wire without this matching loud audit line, so silent fallback
/// is impossible by construction (turing's Q5b verdict).
///
/// Best-effort: write errors are logged-but-swallowed.
pub fn emit_local_fallback(
    state_dir: &Path,
    mol_id: &MoleculeId,
    from_adapter: &str,
    to_adapter: &str,
    cause: &LocalFailureCause,
) {
    let event = EventV2::LocalFallback {
        mol_id: mol_id.clone(),
        from_adapter: from_adapter.to_owned(),
        to_adapter: to_adapter.to_owned(),
        cause: cause.token(),
        fell_back_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Shared best-effort write path — resolves `events.jsonl` under `state_dir`
/// and appends one envelope. A failure is logged at `warn` and swallowed.
fn write_event(state_dir: &Path, event: EventV2) {
    let path = resolve_events_log_path(state_dir);
    if let Err(e) = emit_one(&path, event, None) {
        // Best-effort, never silent: a write failure is surfaced on stderr
        // but never blocks the hot path (seal-not-lock discipline).
        eprintln!(
            "cosmon-state: failed to emit autonomy-guard event to {}: {e}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn read_events(state_dir: &Path) -> Vec<serde_json::Value> {
        let path = resolve_events_log_path(state_dir);
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn remote_egress_opt_in_lands_with_endpoint() {
        let dir = tempdir().unwrap();
        let mol = MoleculeId::new("task-20260530-d8bc").unwrap();
        emit_remote_egress_opt_in(
            dir.path(),
            &mol,
            "claude",
            Some(&RemoteEndpoint::new("api.anthropic.com", 443)),
        );
        let events = read_events(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "remote_egress_opt_in");
        assert_eq!(events[0]["adapter_name"], "claude");
        assert_eq!(events[0]["endpoint_host"], "api.anthropic.com");
        assert_eq!(events[0]["endpoint_port"], 443);
    }

    #[test]
    fn local_fallback_lands_with_cause_and_atom_ordering() {
        let dir = tempdir().unwrap();
        let mol = MoleculeId::new("task-20260530-c089").unwrap();
        // The atom: egress grant THEN fallback, same block, same molecule.
        emit_remote_egress_opt_in(
            dir.path(),
            &mol,
            "claude",
            Some(&RemoteEndpoint::new("api.anthropic.com", 443)),
        );
        emit_local_fallback(
            dir.path(),
            &mol,
            "local",
            "claude",
            &LocalFailureCause::ConnectionRefused,
        );
        let events = read_events(dir.path());
        assert_eq!(events.len(), 2);
        // The egress grant precedes the fallback line — no remote call
        // surfaces without a matching loud audit record.
        assert_eq!(events[0]["type"], "remote_egress_opt_in");
        assert_eq!(events[1]["type"], "local_fallback");
        assert_eq!(events[1]["from_adapter"], "local");
        assert_eq!(events[1]["to_adapter"], "claude");
        assert_eq!(events[1]["cause"], "connection-refused");
    }

    #[test]
    fn local_fallback_other_cause_renders_verbatim() {
        let dir = tempdir().unwrap();
        let mol = MoleculeId::new("task-20260530-c089").unwrap();
        emit_local_fallback(
            dir.path(),
            &mol,
            "local",
            "openai",
            &LocalFailureCause::Other("grammar-deadlock".to_owned()),
        );
        let events = read_events(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "local_fallback");
        assert_eq!(events[0]["cause"], "grammar-deadlock");
    }

    #[test]
    fn egress_unenforceable_lands_with_reason() {
        let dir = tempdir().unwrap();
        let mol = MoleculeId::new("task-20260712-8d2d").unwrap();
        emit_egress_unenforceable(
            dir.path(),
            &mol,
            "local",
            "deny-external cannot be kernel-enforced on this host",
        );
        let events = read_events(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "egress_unenforceable");
        assert_eq!(events[0]["adapter_name"], "local");
        assert_eq!(
            events[0]["reason"],
            "deny-external cannot be kernel-enforced on this host"
        );
    }

    #[test]
    fn local_exec_receipt_lands_positive() {
        let dir = tempdir().unwrap();
        let mol = MoleculeId::new("task-20260530-d8bc").unwrap();
        let receipt = LocalExecReceipt::new(true, 42.0, 0.8);
        emit_local_exec_receipt(dir.path(), &mol, 3, &receipt);
        let events = read_events(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "local_exec_receipt");
        assert_eq!(events[0]["turn"], 3);
        assert_eq!(events[0]["ffi_receipt"], true);
        assert_eq!(events[0]["throughput_band"], "local");
    }
}
