// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-Adapter unit tests (ADR-098 / C8 Tier-1).
//!
//! For each silent-failure mode WS-1 through WS-5, both Adapters
//! (claude, aider) emit a structurally equivalent [`EventV2`] variant
//! differing only in `adapter_name`. Tests exercise the emission
//! infrastructure directly (free helpers in
//! [`cosmon_state::events::worker_spawn`]) and the Adapter-owned
//! `consume_briefing` path that wraps it — no tmux, no real CLI
//! binaries. The Tier-1 leg of the Trigger #1 / #3 detectors named
//! in ADR-098 §6 lives here.

use std::fs;
use std::path::Path;

use chrono::Utc;
use cosmon_core::event_v2::{
    AdapterHandleState, AdapterProbeKind, AdapterProbeResult, Envelope, EventV2,
    PerturbationChannel,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_state::events::worker_spawn as ws;
use cosmon_transport::aider;
use cosmon_transport::claude;
use cosmon_transport::spawn::AdapterTelemetry;
use tempfile::{tempdir, TempDir};

const ADAPTERS: [&str; 2] = [claude::ADAPTER_NAME, aider::ADAPTER_NAME];

fn mol() -> MoleculeId {
    MoleculeId::new("task-20260517-4f15").unwrap()
}

fn wkr(name: &str) -> WorkerId {
    WorkerId::new(name).unwrap()
}

fn envelopes(dir: &Path) -> Vec<Envelope> {
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Envelope::from_line(l).expect("envelope must parse"))
        .collect()
}

/// Drive every Worker-Spawn Port emit-site once per Adapter into a
/// fresh tempdir. Used by the WS-N per-mode assertions below; combining
/// the five emit calls into a fixture keeps the budget tight while
/// preserving call-site stability with the five ADR-079 §5 obligations.
///
/// **Happy-path lineage now covers all five WS-* variants.** WS-4
/// (`AdapterBriefingConsumed`) was added so the cat-test asserts
/// `⊇ {ws1..ws5}` rather than the four-variant subset that left WS-4
/// silent.
fn drive_all_modes(adapter: &str) -> (TempDir, MoleculeId, WorkerId) {
    let dir = tempdir().unwrap();
    let mol_id = mol();
    let w = wkr("polecat-drive");
    let now = Utc::now();
    ws::emit_worker_spawn_attempted(dir.path(), &mol_id, &w, adapter, "/wt", "uuid", 0, None);
    ws::emit_adapter_liveness_probed(
        dir.path(),
        &mol_id,
        &w,
        adapter,
        AdapterProbeKind::PaneSignature,
        AdapterProbeResult::Alive {
            evidence: "alive".into(),
        },
        0,
    );
    ws::emit_adapter_pane_signature_checked(
        dir.path(),
        &mol_id,
        &w,
        adapter,
        &[adapter.into()],
        "bash",
        false,
        PerturbationChannel::Propulsion,
    );
    ws::emit_adapter_briefing_consumed(
        dir.path(),
        &mol_id,
        &w,
        adapter,
        "briefing.md",
        "obs",
        "rec",
        0,
        now,
    );
    ws::emit_adapter_handle_reconciled(
        dir.path(),
        &mol_id,
        &w,
        adapter,
        AdapterHandleState::ReleasedOrphan,
        Some(now),
        now,
        0,
    );
    (dir, mol_id, w)
}

/// WS-1 — both Adapters emit `WorkerSpawnAttempted`; a second emit
/// under the same `(mol_id, worker_id)` with `pre_existing_worker =
/// Some(_)` records the double-spawn collision (the audit signal).
#[test]
fn ws1_worker_spawn_attempted_and_double_spawn_collision() {
    for adapter in ADAPTERS {
        let dir = tempdir().unwrap();
        let m = mol();
        let w = wkr("polecat-collide");
        ws::emit_worker_spawn_attempted(dir.path(), &m, &w, adapter, "/wt", "uuid-1", 0, None);
        ws::emit_worker_spawn_attempted(dir.path(), &m, &w, adapter, "/wt", "uuid-2", 0, Some(&w));
        let attempts: Vec<_> = envelopes(dir.path())
            .into_iter()
            .filter_map(|e| match e.event {
                EventV2::WorkerSpawnAttempted {
                    adapter_name,
                    pre_existing_worker,
                    ..
                } => Some((adapter_name, pre_existing_worker)),
                _ => None,
            })
            .collect();
        assert_eq!(attempts.len(), 2, "{adapter}: two attempts on disk");
        assert!(attempts.iter().all(|(a, _)| a == adapter));
        assert!(attempts[0].1.is_none() && attempts[1].1.is_some());
    }
}

/// WS-2 — both Adapters emit `AdapterLivenessProbed` with a tagged
/// `Alive` / `Stuck` verdict; the `adapter_name` is the discriminator.
#[test]
fn ws2_liveness_probed_carries_adapter_name_and_verdict() {
    for adapter in ADAPTERS {
        let (dir, _, _) = drive_all_modes(adapter);
        let probe = envelopes(dir.path())
            .into_iter()
            .find_map(|e| match e.event {
                EventV2::AdapterLivenessProbed {
                    adapter_name,
                    probe_kind,
                    probe_result,
                    ..
                } => Some((adapter_name, probe_kind, probe_result)),
                _ => None,
            })
            .expect("AdapterLivenessProbed");
        assert_eq!(probe.0, adapter);
        assert_eq!(probe.1, AdapterProbeKind::PaneSignature);
        assert!(matches!(probe.2, AdapterProbeResult::Alive { .. }));
    }
}

/// WS-3 — both Adapters emit `AdapterPaneSignatureChecked` with
/// `matched = false` and the perturbation `channel` recorded. A wrong-
/// process write becomes auditable through this variant.
#[test]
fn ws3_pane_signature_checked_records_mismatch_and_channel() {
    for adapter in ADAPTERS {
        let (dir, _, _) = drive_all_modes(adapter);
        let check = envelopes(dir.path())
            .into_iter()
            .find_map(|e| match e.event {
                EventV2::AdapterPaneSignatureChecked {
                    adapter_name,
                    matched,
                    channel,
                    ..
                } => Some((adapter_name, matched, channel)),
                _ => None,
            })
            .expect("AdapterPaneSignatureChecked");
        assert_eq!(check.0, adapter);
        assert!(!check.1);
        assert_eq!(check.2, PerturbationChannel::Propulsion);
    }
}

/// WS-4 — both Adapters' `consume_briefing` emit
/// `AdapterBriefingConsumed` with observed seal = hash(bytes-read);
/// disagreement with `recorded_seal` is the silent-failure signal. The
/// absence of this event is the WS-4 detector (per-Adapter mod tests
/// `missing_consume_briefing_yields_empty_audit_match` cover the
/// negative case; this is the positive symmetry control).
#[test]
fn ws4_briefing_consumed_observed_seal_for_both_adapters() {
    let dir = tempdir().unwrap();
    let briefing = dir.path().join("briefing.md");
    fs::write(&briefing, b"same bytes").unwrap();
    let expected = blake3::hash(b"same bytes").to_hex().to_string();

    let t = |w| AdapterTelemetry::new(mol(), wkr(w), dir.path().to_owned(), "uuid");
    claude::consume_briefing(&briefing, "rec-c", Some(&t("polecat-c4-c"))).expect("claude");
    aider::consume_briefing(&briefing, "rec-a", Some(&t("polecat-c4-a"))).expect("aider");

    let consumed: Vec<_> = envelopes(dir.path())
        .into_iter()
        .filter_map(|e| match e.event {
            EventV2::AdapterBriefingConsumed {
                adapter_name,
                briefing_seal_observed,
                briefing_seal_recorded,
                ..
            } => Some((adapter_name, briefing_seal_observed, briefing_seal_recorded)),
            _ => None,
        })
        .collect();
    assert_eq!(consumed.len(), 2);
    for (_, observed, _) in &consumed {
        assert_eq!(observed, &expected);
    }
    assert!(consumed
        .iter()
        .any(|(a, _, r)| a == "claude" && r == "rec-c"));
    assert!(consumed
        .iter()
        .any(|(a, _, r)| a == "aider" && r == "rec-a"));
}

/// WS-5 — both Adapters emit `AdapterHandleReconciled` with the final
/// `handle_state` (`ReleasedClean` / `ReleasedOrphan` round-trip
/// identically on either Adapter through the same helper).
#[test]
fn ws5_handle_reconciled_records_state() {
    for adapter in ADAPTERS {
        let (dir, _, _) = drive_all_modes(adapter);
        let rec = envelopes(dir.path())
            .into_iter()
            .find_map(|e| match e.event {
                EventV2::AdapterHandleReconciled {
                    adapter_name,
                    handle_state,
                    ..
                } => Some((adapter_name, handle_state)),
                _ => None,
            })
            .expect("AdapterHandleReconciled");
        assert_eq!(rec.0, adapter);
        assert_eq!(rec.1, AdapterHandleState::ReleasedOrphan);
    }
}

/// Trigger #1 detector — both Adapters walking the same Port path
/// emit an identical variant sequence modulo `adapter_name`. A future
/// Adapter that adds a sixth variant — or skips one of the five —
/// surfaces here as a sequence mismatch at the unit
/// perimeter.
///
/// **`kinds_of` panics on unknown variants.** An earlier helper
/// collapsed unknown variants to the uniform string `"other"`, which
/// silently passed when a future sixth WS-* landed on one Adapter and
/// not the other. The current helper panics — an unknown variant is a
/// test failure, not a pass-through.
///
/// **Happy-path lineage.** The
/// assertion is `kinds_of(adapter) ⊇ {ws1..ws5}` — the lineage of
/// every spawn-seam emit-site must land on disk, in adapter-symmetric
/// order, on the happy path. The set-containment form (rather than a
/// four-element `assert_eq!`) makes the contract explicit: a
/// future seventh WS-* event is welcome, a missing one is a regression.
#[test]
fn cross_adapter_variant_sequence_is_symmetric_modulo_adapter_name() {
    let kinds_of = |dir: &Path| -> Vec<&'static str> {
        envelopes(dir)
            .iter()
            .map(|e| match e.event {
                EventV2::WorkerSpawnAttempted { .. } => "ws1",
                EventV2::AdapterLivenessProbed { .. } => "ws2",
                EventV2::AdapterPaneSignatureChecked { .. } => "ws3",
                EventV2::AdapterBriefingConsumed { .. } => "ws4",
                EventV2::AdapterHandleReconciled { .. } => "ws5",
                EventV2::WorkerSpawnFailed { .. } => "ws1_failed",
                EventV2::WorkerSpawnRolledBack { .. } => "ws1_rolled_back",
                ref other => panic!(
                    "cross-adapter cat-test saw an unmapped EventV2 variant \
                     in the WS-* perimeter: {other:?}. Add an arm to \
                     `kinds_of` rather than collapsing to 'other' — silent \
                     pass-through is exactly what delib-20260519-e6db W3 / \
                     adversary F1.1 names as the failure mode",
                ),
            })
            .collect()
    };
    let (claude_dir, _, _) = drive_all_modes(claude::ADAPTER_NAME);
    let (aider_dir, _, _) = drive_all_modes(aider::ADAPTER_NAME);
    let claude_kinds = kinds_of(claude_dir.path());
    let aider_kinds = kinds_of(aider_dir.path());
    assert_eq!(
        claude_kinds, aider_kinds,
        "both Adapters must emit the same variant sequence modulo adapter_name"
    );
    // Containment form (delib-20260519-e6db W3): the happy-path lineage
    // must cover *all five* WS-* variants. A missing variant is a
    // silent-failure signature; extra variants (future WS-6, …) are
    // welcome.
    for required in ["ws1", "ws2", "ws3", "ws4", "ws5"] {
        assert!(
            claude_kinds.contains(&required),
            "happy-path lineage must contain {required}; observed {claude_kinds:?}"
        );
    }
}
