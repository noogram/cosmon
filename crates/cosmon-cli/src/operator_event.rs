// SPDX-License-Identifier: AGPL-3.0-only

//! `operator.*` event emission — STREAM half of layer B compromise.
//!
//! The CLI itself (not the operator) writes typed `operator.*` envelopes
//! to `events.jsonl` on every interactive invocation. Two pairings the
//! downstream patrol cares about:
//!
//! - `OperatorPresent` / `OperatorAbsent` — chalk-mark for sessions
//!   touching the CLI; carries [`PresenceSource`] so destructive-action
//!   gating can apply the no-cloning theorem (ADR §F3).
//! - `OperatorSpark` / `OperatorVerdict` — joined by `spark_id` to
//!   derive **latency** of operator attention. The single most
//!   important liveness property (einstein §4).
//!
//! Plus `OperatorSigned` for destructive verbs (`cs done`, `cs collapse`,
//! `cs purge`, …) — observed today, gated tomorrow if the 2-week Mach
//! test confirms the proxy works.
//!
//! # Defensive emission
//!
//! Every helper here is *best-effort*: a serialise or write failure is
//! silently swallowed, the hot path proceeds. Same trace-not-lock
//! discipline as briefing seals (ADR-047) — telemetry must never break
//! the operator's command.
//!
//! # No-op when not in a cosmon repo
//!
//! When `.cosmon/state/` does not exist (pre-init directory, foreign
//! repo) the helpers no-op silently. `cs init` is the canonical
//! moment that creates the substrate; before then there is nowhere
//! to write.

use std::path::Path;

use chrono::Utc;
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::presence_sensor::PresenceSource;
use cosmon_state::event_log::{emit_one, resolve_events_log_path};

/// Honour the `COSMON_NO_OPERATOR_EVENTS` env-var opt-out — when
/// set to a non-empty value, every helper in this module no-ops.
///
/// Used by integration tests that snapshot `events.jsonl` (e.g.
/// `cs migrate to solo` fixture seals) and need byte-exact ledger
/// stability across CLI invocations. The opt-out is a deliberate
/// escape hatch — same trace-not-lock discipline as briefing seals
/// (ADR-047): emission is best-effort, opt-out is best-effort, the
/// hot path proceeds either way.
fn emission_disabled() -> bool {
    std::env::var("COSMON_NO_OPERATOR_EVENTS")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Best-effort emit of [`EventV2::OperatorPresent`].
///
/// Called once per interactive `cs` invocation, immediately after the
/// state directory is resolved. The event records that **a CLI was
/// invoked from this session at this wall-clock time** — the
/// foundational signal the `operator-attention-patrol` proxy reads
/// to detect stalls.
///
/// `source` defaults to [`PresenceSource::Internal`] for the V0
/// stream-only emission: cosmon's own act of writing the event is
/// the substrate — tautological with respect to the agent, and the
/// no-cloning theorem prevents downstream destructive-action gating
/// from trusting it. Exogenous-sensor wiring (`IoregSensor::poll()`
/// →`PresenceSource::Ioreg`) lands in a follow-up molecule.
///
/// No-op when `state_dir` does not exist yet (pre-`cs init`) OR
/// when `COSMON_NO_OPERATOR_EVENTS` is set.
pub fn emit_operator_present(
    state_dir: &Path,
    sid: &str,
    nucleon_id: Option<&str>,
    orbitale_id: Option<&str>,
    source: PresenceSource,
) {
    if emission_disabled() || !state_dir.exists() {
        return;
    }
    let event = EventV2::OperatorPresent {
        sid: sid.to_owned(),
        nucleon_id: nucleon_id.map(str::to_owned),
        orbitale_id: orbitale_id.map(str::to_owned),
        phase: "Biological".to_owned(),
        ts: Utc::now(),
        source,
    };
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = emit_one(path, event, None);
}

/// Best-effort emit of [`EventV2::OperatorSigned`] — record that the
/// operator authorised a destructive or otherwise authoritative
/// action (`cs done`, `cs collapse`, `cs purge`, `git push`, `rm`, …).
///
/// V0 records the gesture; gating destructive actions on signed events
/// is explicitly deferred (the Mach gate). After
/// 2 weeks of trace data the operator decides whether to wire gating
/// on this signal.
///
/// `signature_method` is free-form so future signing substrates
/// (touch-id, yubikey, …) drop in without a schema change.
pub fn emit_operator_signed(
    state_dir: &Path,
    action: &str,
    mol_id: Option<&MoleculeId>,
    signature_method: &str,
) {
    if emission_disabled() || !state_dir.exists() {
        return;
    }
    let event = EventV2::OperatorSigned {
        action: action.to_owned(),
        mol_id: mol_id.cloned(),
        signature_method: signature_method.to_owned(),
    };
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = emit_one(path, event, None);
}

/// Best-effort emit of [`EventV2::OperatorSpark`] — a request that
/// asks the system for a verdict (`cs ask`, `cs spark`, inbound
/// whisper, …).
///
/// Pairs with `emit_operator_verdict` / `emit_operator_refused`
/// via `spark_id` so a downstream consumer can derive the
/// spark→verdict latency. `content_hash` should be a content-
/// addressed digest (BLAKE3 hex prefix is the conventional shape);
/// the spark text itself lives in the channel-specific store
/// (whisper inbox, chat transcript, …).
pub fn emit_operator_spark(
    state_dir: &Path,
    spark_id: &str,
    src: &str,
    content_hash: &str,
    ttl_h: Option<u64>,
    mol_ref: Option<&MoleculeId>,
) {
    if emission_disabled() || !state_dir.exists() {
        return;
    }
    let event = EventV2::OperatorSpark {
        spark_id: spark_id.to_owned(),
        src: src.to_owned(),
        content_hash: content_hash.to_owned(),
        ttl_h,
        mol_ref: mol_ref.cloned(),
    };
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = emit_one(path, event, None);
}

/// Resolve a session id for the current invocation.
///
/// Order of preference:
///
/// 1. `COSMON_SESSION_ID` env var — set by long-lived shells / pilot
///    sessions that want to attribute every emission to a stable id.
/// 2. `TMUX_PANE` — the tmux pane id (e.g. `%23`) is a reasonable
///    proxy when running inside a tmux session.
/// 3. `cli-<pid>` — last-resort fallback so the event always carries
///    *some* sid; `pid` is per-invocation and shows up in the trace
///    as a coarse bucket of one-shot CLI calls.
#[must_use]
pub fn current_session_id() -> String {
    if let Ok(sid) = std::env::var("COSMON_SESSION_ID") {
        if !sid.is_empty() {
            return sid;
        }
    }
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        if !pane.is_empty() {
            return format!("tmux:{pane}");
        }
    }
    format!("cli-{pid}", pid = std::process::id())
}

/// Resolve an optional Nucléon id for the current invocation.
///
/// Reads `COSMON_NUCLEON_ID` env var (set by the pilot session
/// substrate, ADR-061). Returns `None` if not present so the event
/// stays informative-only — a missing nucleon id is not an error.
#[must_use]
pub fn current_nucleon_id() -> Option<String> {
    std::env::var("COSMON_NUCLEON_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Resolve an optional Orbitale id for the current invocation.
///
/// Reads `COSMON_ORBITALE_ID` env var (set by the pilot session
/// substrate, ADR-063). `hostname` is **not** used as a fallback —
/// the Orbitale identity is opt-in by design.
#[must_use]
pub fn current_orbitale_id() -> Option<String> {
    std::env::var("COSMON_ORBITALE_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::Envelope;

    #[test]
    fn emit_present_writes_one_line() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        emit_operator_present(
            &state_dir,
            "cli-9999",
            Some("nucleon-you"),
            None,
            PresenceSource::Internal,
        );

        let events_path = resolve_events_log_path(&state_dir);
        let lines = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            lines.contains(r#""type":"operator_present""#),
            "lines={lines}"
        );
        let env = Envelope::from_line(lines.trim()).unwrap();
        match env.event {
            EventV2::OperatorPresent { sid, source, .. } => {
                assert_eq!(sid, "cli-9999");
                assert!(!source.is_exogenous(), "Internal must not be exogenous");
            }
            other => panic!("expected OperatorPresent, got {other:?}"),
        }
    }

    #[test]
    fn emit_signed_includes_molecule_id() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let mol = MoleculeId::new("cs-20260509-aaaa").unwrap();
        emit_operator_signed(&state_dir, "cs done", Some(&mol), "shell");

        let events_path = resolve_events_log_path(&state_dir);
        let line = std::fs::read_to_string(&events_path).unwrap();
        let env = Envelope::from_line(line.trim()).unwrap();
        match env.event {
            EventV2::OperatorSigned { action, mol_id, .. } => {
                assert_eq!(action, "cs done");
                assert_eq!(mol_id.unwrap().as_str(), "cs-20260509-aaaa");
            }
            other => panic!("expected OperatorSigned, got {other:?}"),
        }
    }

    #[test]
    fn helpers_noop_when_state_dir_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("nonexistent");
        // Must not panic, must not create the directory.
        emit_operator_present(&state_dir, "sid", None, None, PresenceSource::Internal);
        emit_operator_signed(&state_dir, "rm", None, "shell");
        emit_operator_spark(&state_dir, "sp", "cli", "h", None, None);
        assert!(!state_dir.exists());
    }

    #[test]
    fn current_session_id_falls_back_to_pid() {
        // Clear the relevant env vars for the test.
        unsafe {
            std::env::remove_var("COSMON_SESSION_ID");
            std::env::remove_var("TMUX_PANE");
        }
        let sid = current_session_id();
        assert!(sid.starts_with("cli-"), "sid={sid}");
    }
}
