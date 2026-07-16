// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-Adapter smoke test (ADR-098 / C8 Tier-2).
//!
//! Requires the `claude` and `aider` binaries on PATH plus a real
//! `tmux`. Gated with `#[ignore]`; CI nightly runs with
//! `continue-on-error: true` (ADR-098 §6 / C9 — the 90-day forensic
//! gate consumes the resulting `events.jsonl`, not the per-run pass).
//!
//! Invocation:
//!
//! ```bash
//! which claude aider tmux
//! cargo test -p cosmon-transport -- --ignored cross_adapter
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::event_v2::{Envelope, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_transport::aider::{self, AiderPermissionFlags, AiderSessionConfig};
use cosmon_transport::claude::{self, ClaudeSessionConfig, PermissionMode};
use cosmon_transport::spawn::AdapterTelemetry;
use tempfile::tempdir;

const TEST_SOCKET: &str = "cosmon-cross-adapter-tier2";

fn cleanup() {
    let _ = Command::new("tmux")
        .args(["-L", TEST_SOCKET, "kill-server"])
        .output();
}

fn require_binary(name: &str) {
    let ok = Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "Tier-2 smoke requires `{name}` on PATH");
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trivial_briefing.md")
}

fn envelopes(state_dir: &Path) -> Vec<Envelope> {
    let raw = fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Envelope::from_line(l).expect("envelope must parse"))
        .collect()
}

/// **Artefact equivalence** (galileo §8.1 Trigger #1) — pin the
/// fixture's structural contract. The driven-agent loop (C5/C6)
/// consumes this fixture to produce `output.md`; until that loop
/// lands the contract regression is what this test catches. Model
/// output is intentionally not asserted (non-deterministic by design).
#[test]
#[ignore = "requires real claude+aider binaries + tmux"]
fn same_briefing_both_adapters_produce_structurally_equivalent_artefacts() {
    let content =
        fs::read_to_string(fixture_path()).expect("trivial_briefing.md fixture must ship in tree");
    assert!(content.contains("hello from"), "structural contract token");
    assert!(content.contains("$MOLECULE_DIR/output.md"), "artefact path");
    assert!(content.contains("`cs evolve`"), "cs evolve obligation");
}

/// **Event-lineage equivalence modulo `adapter_name`** (galileo §8.3
/// Trigger #3) — both Adapters walk a real spawn-then-kill cycle; the
/// resulting Worker-Spawn Port `EventV2` sequence on
/// `state_dir/events.jsonl` carries the full happy-path WS-1..WS-5
/// lineage for each Adapter.
///
/// **`⊇ {ws1..ws5}`.** An earlier
/// check only asserted `["ws1", "ws5"]` were *contained* in
/// the trail. A worker that segfaulted between spawn and probe emitted
/// exactly those two events for both Adapters and silently passed.
/// The tightened check pins the
/// full FSM-prescribed superset: WS-1, WS-2, WS-3, WS-4, WS-5 must
/// all be present, in FSM order. The early-kill skeleton-run case
/// lives in [`cross_adapter_event_lineage_early_kill_emits_terminal`]
/// below.
#[test]
#[ignore = "requires real claude+aider binaries + tmux"]
fn cross_adapter_event_lineage_equivalence_modulo_adapter_name() {
    require_binary("tmux");
    require_binary("claude");
    require_binary("aider");

    cleanup();
    let dir = tempdir().unwrap();
    let state = dir.path();
    let workdir = dir.path().to_string_lossy().into_owned();
    let mol_id = MoleculeId::new("task-20260517-4f15").unwrap();

    let wc = WorkerId::new("smoke-claude").unwrap();
    let tc = AdapterTelemetry::new(mol_id.clone(), wc.clone(), state.to_owned(), "uuid-c");
    let _ = claude::spawn_claude_session(&ClaudeSessionConfig {
        socket: TEST_SOCKET.into(),
        session_name: wc.as_str().into(),
        work_dir: workdir.clone(),
        permission_mode: PermissionMode::Plan,
        prompt: Some("noop".into()),
        telemetry: Some(tc.clone()),
        pre_existing_worker: None,
    });
    let _ = claude::kill_session(TEST_SOCKET, wc.as_str(), Some(&tc));

    let wa = WorkerId::new("smoke-aider").unwrap();
    let ta = AdapterTelemetry::new(mol_id, wa.clone(), state.to_owned(), "uuid-a");
    let _ = aider::spawn_aider_session(&AiderSessionConfig {
        socket: TEST_SOCKET.into(),
        session_name: wa.as_str().into(),
        work_dir: workdir,
        permission_flags: AiderPermissionFlags::Plan,
        model: "kimi-k2.6".into(),
        prompt: Some("noop".into()),
        extra_args: Vec::new(),
        telemetry: Some(ta.clone()),
        pre_existing_worker: None,
    });
    let _ = aider::kill_session(TEST_SOCKET, wa.as_str(), Some(&ta));
    cleanup();

    let events = envelopes(state);
    let kinds_for = |adapter: &str| -> Vec<&'static str> {
        events
            .iter()
            .filter_map(|e| match &e.event {
                EventV2::WorkerSpawnAttempted { adapter_name, .. } if adapter_name == adapter => {
                    Some("ws1")
                }
                EventV2::AdapterLivenessProbed { adapter_name, .. } if adapter_name == adapter => {
                    Some("ws2")
                }
                EventV2::AdapterPaneSignatureChecked { adapter_name, .. }
                    if adapter_name == adapter =>
                {
                    Some("ws3")
                }
                EventV2::AdapterBriefingConsumed { adapter_name, .. }
                    if adapter_name == adapter =>
                {
                    Some("ws4")
                }
                EventV2::AdapterHandleReconciled { adapter_name, .. }
                    if adapter_name == adapter =>
                {
                    Some("ws5")
                }
                _ => None,
            })
            .collect()
    };
    let expected: &[&str] = &["ws1", "ws2", "ws3", "ws4", "ws5"];
    for adapter in ["claude", "aider"] {
        let seq = kinds_for(adapter);
        for ws in expected {
            assert!(
                seq.contains(ws),
                "{adapter}: happy-path lineage must contain {ws}; observed {seq:?}",
            );
        }
        // Order check: the FSM-prescribed sequence WS-1..WS-5 must
        // appear as a subsequence of the observed events. Catches the
        // adversary F1.1 failure where the four were present but
        // out-of-order (e.g. WS-5 before WS-4 = release-before-consume).
        let observed: Vec<&str> = seq
            .iter()
            .filter(|k| expected.contains(k))
            .copied()
            .collect();
        let mut idx = 0;
        for k in &observed {
            if *k == expected[idx] {
                idx += 1;
                if idx == expected.len() {
                    break;
                }
            }
        }
        assert_eq!(
            idx,
            expected.len(),
            "{adapter}: WS-1..WS-5 must appear in FSM order; observed {observed:?}"
        );
    }
    assert_eq!(kinds_for("claude"), kinds_for("aider"));
}

/// **Skeleton-run shape.**
///
/// Driven complement to the happy-path assertion above: when a
/// spawn-then-immediate-kill cycle never reaches probe / briefing
/// consumption, the trail must still close cleanly. WS-1 + WS-5 are
/// present; WS-4 is **absent**. The pre-W3 contains-check accepted
/// this shape as success because it only required WS-1 and WS-5; now
/// we name the shape explicitly so a future regression where the kill
/// races the consume-briefing path stays diagnosable.
#[test]
#[ignore = "requires real claude+aider binaries + tmux"]
fn cross_adapter_event_lineage_early_kill_emits_terminal_without_consume() {
    require_binary("tmux");
    require_binary("claude");

    cleanup();
    let dir = tempdir().unwrap();
    let state = dir.path();
    let workdir = dir.path().to_string_lossy().into_owned();
    let mol_id = MoleculeId::new("task-20260517-4f15").unwrap();

    let w = WorkerId::new("smoke-early-kill").unwrap();
    let t = AdapterTelemetry::new(mol_id, w.clone(), state.to_owned(), "uuid-early");
    let _ = claude::spawn_claude_session(&ClaudeSessionConfig {
        socket: TEST_SOCKET.into(),
        session_name: w.as_str().into(),
        work_dir: workdir,
        permission_mode: PermissionMode::Plan,
        prompt: Some("noop".into()),
        telemetry: Some(t.clone()),
        pre_existing_worker: None,
    });
    // Immediate kill — no probe, no consume_briefing.
    let _ = claude::kill_session(TEST_SOCKET, w.as_str(), Some(&t));
    cleanup();

    let events = envelopes(state);
    let has = |pred: fn(&EventV2) -> bool| -> bool { events.iter().any(|e| pred(&e.event)) };
    assert!(
        has(|e| matches!(e, EventV2::WorkerSpawnAttempted { .. })),
        "early-kill must still emit WS-1"
    );
    assert!(
        has(|e| matches!(e, EventV2::AdapterHandleReconciled { .. })),
        "early-kill must still emit WS-5"
    );
    assert!(
        !has(|e| matches!(e, EventV2::AdapterBriefingConsumed { .. })),
        "early-kill must NOT emit WS-4 — that would mean the adapter \
         consumed briefing.md before the kill, which the test never \
         triggered"
    );
}
