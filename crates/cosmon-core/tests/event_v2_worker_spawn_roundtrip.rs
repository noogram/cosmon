// SPDX-License-Identifier: AGPL-3.0-only

//! Serde round-trip tests for the five Worker-Spawn Port `EventV2`
//! variants introduced by ADR-097.
//!
//! Each test pins down two properties:
//!
//! 1. The variant serialises to JSON and deserialises back to a value
//!    equal to the original (the IFBDD trail survives a write-read
//!    cycle through `events.jsonl`).
//! 2. The `adapter_name` field is populated from the
//!    `#[serde(default = "default_adapter_name")]` shim when absent
//!    from a legacy line — backwards compat for the (intentionally
//!    short) window during which the variants existed in shadow form.
//!
//! These tests stand alongside the in-module tests in
//! `cosmon_state::events::worker_spawn::tests` (which exercise the
//! *emission helpers*) and the audit-query negative tests there. Each
//! layer is a different sensor on the same invariant; together they
//! form the IFBDD trail the briefing demands.

use chrono::Utc;
use cosmon_core::event_v2::{
    AdapterHandleState, AdapterProbeKind, AdapterProbeResult, EventV2, PerturbationChannel,
};
use cosmon_core::id::{MoleculeId, WorkerId};

fn mol() -> MoleculeId {
    MoleculeId::new("task-20260517-0b46").unwrap()
}

fn wkr() -> WorkerId {
    WorkerId::new("polecat-aaaa").unwrap()
}

/// WS-1: `WorkerSpawnAttempted` round-trips through serde without
/// losing the `pre_existing_worker` collision marker.
#[test]
fn worker_spawn_attempted_serde_roundtrip() {
    let other = WorkerId::new("polecat-bbbb").unwrap();
    let original = EventV2::WorkerSpawnAttempted {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        worktree_path: "/tmp/wt".to_owned(),
        invocation_uuid: "uuid-xyz".to_owned(),
        pid: 12345,
        pre_existing_worker: Some(other.clone()),
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// Legacy `events.jsonl` lines written before the `adapter_name` field
/// landed (during the shadow-shape grace window) must deserialise to
/// the `default_adapter_name` ("claude") rather than failing.
#[test]
fn worker_spawn_attempted_adapter_name_default() {
    let legacy_json = serde_json::json!({
        "type": "worker_spawn_attempted",
        "mol_id": "task-20260517-0b46",
        "worker_id": "polecat-aaaa",
        "worktree_path": "/tmp/wt",
        "invocation_uuid": "uuid",
        "pid": 0,
    })
    .to_string();
    let parsed: EventV2 = serde_json::from_str(&legacy_json).unwrap();
    let EventV2::WorkerSpawnAttempted { adapter_name, .. } = parsed else {
        panic!("expected WorkerSpawnAttempted");
    };
    assert_eq!(adapter_name, "claude");
}

/// WS-2: `AdapterLivenessProbed` round-trips through serde with the
/// `Alive { evidence }` payload preserved.
#[test]
fn adapter_liveness_probed_serde_roundtrip_alive() {
    let original = EventV2::AdapterLivenessProbed {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        probe_kind: AdapterProbeKind::PaneSignature,
        probe_result: AdapterProbeResult::Alive {
            evidence: "pane fg=claude".to_owned(),
        },
        elapsed_since_last_advance_ms: 4_200,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// WS-2 mirror: the same round-trip for the `Stuck { reason }` payload.
#[test]
fn adapter_liveness_probed_serde_roundtrip_stuck() {
    let original = EventV2::AdapterLivenessProbed {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        probe_kind: AdapterProbeKind::ProcessExit,
        probe_result: AdapterProbeResult::Stuck {
            reason: "no token advance for 120s".to_owned(),
        },
        elapsed_since_last_advance_ms: 120_000,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// WS-3: `AdapterPaneSignatureChecked` round-trips through serde,
/// preserving the `channel` discriminator so a downstream audit can
/// distinguish a propulsion mismatch from a whisper mismatch.
#[test]
fn adapter_pane_signature_checked_serde_roundtrip() {
    let original = EventV2::AdapterPaneSignatureChecked {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        registered_signature: vec!["claude".to_owned(), "claude-code".to_owned()],
        observed_command: "claude".to_owned(),
        matched: true,
        channel: PerturbationChannel::Whisper,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// WS-4: `AdapterBriefingConsumed` round-trips through serde,
/// preserving both observed and recorded seals. The audit relies on
/// the *separation* of the two fields to detect shadow-contract drift.
#[test]
fn adapter_briefing_consumed_serde_roundtrip() {
    let observed_at = Utc::now();
    let original = EventV2::AdapterBriefingConsumed {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        briefing_path: "briefing.md".to_owned(),
        briefing_seal_observed: "a".repeat(64),
        briefing_seal_recorded: "b".repeat(64),
        bytes_read: 2_048,
        consumed_at: observed_at,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// WS-5: `AdapterHandleReconciled` round-trips through serde with a
/// non-zero `gap_ms` and the `ReleasedOrphan` discriminator preserved.
#[test]
fn adapter_handle_reconciled_serde_roundtrip_orphan() {
    let exit = Utc::now();
    let release = exit + chrono::Duration::milliseconds(750);
    let original = EventV2::AdapterHandleReconciled {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        handle_state: AdapterHandleState::ReleasedOrphan,
        underlying_exit_observed_at: Some(exit),
        handle_released_at: release,
        gap_ms: 750,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// WS-5 mirror: when `underlying_exit_observed_at` is `None`, the field
/// is omitted from the wire form (matches the `skip_serializing_if`
/// attribute) and round-trips back to `None`.
#[test]
fn adapter_handle_reconciled_serde_roundtrip_unobserved_exit() {
    let release = Utc::now();
    let original = EventV2::AdapterHandleReconciled {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "claude".to_owned(),
        handle_state: AdapterHandleState::Held,
        underlying_exit_observed_at: None,
        handle_released_at: release,
        gap_ms: 0,
    };
    let json = serde_json::to_string(&original).unwrap();
    assert!(
        !json.contains("underlying_exit_observed_at"),
        "field should be omitted when None: {json}"
    );
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

// --- SF-1..SF-5 (ADR-100 / R2 wave 2) -------------------------------------
//
// The five `SF*` variants encode silent-failure modes that Direct-API
// Adapters (OpenAI / Anthropic / xAI) must emit before they can be
// considered IFBDD-compliant. The fields are shared across the five
// (`provider_name`, `adapter_name`, `model_name`, `trigger_context`,
// `recovery_attempted`) plus variant-specific forensic detail; the
// round-trip tests below pin that the shared/specific split survives
// a write-read cycle through `events.jsonl`.

/// SF-1: `SF1HttpTransport` round-trips through serde with retry
/// metadata preserved.
#[test]
fn sf1_http_transport_serde_roundtrip() {
    let original = EventV2::SF1HttpTransport {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "openai-direct".to_owned(),
        provider_name: "openai".to_owned(),
        model_name: Some("gpt-4o".to_owned()),
        trigger_context: "completion".to_owned(),
        recovery_attempted: false,
        retry_count: 5,
        error_class: "connection_refused".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(
        json.contains("\"type\":\"sf1_http_transport\""),
        "expected snake_case discriminator: {json}"
    );
}

/// SF-1 mirror: `model_name = None` is omitted from the wire form
/// (preserves the `skip_serializing_if` invariant).
#[test]
fn sf1_http_transport_omits_unknown_model() {
    let original = EventV2::SF1HttpTransport {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "openai-direct".to_owned(),
        provider_name: "openai".to_owned(),
        model_name: None,
        trigger_context: "discovery".to_owned(),
        recovery_attempted: false,
        retry_count: 0,
        error_class: "dns".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    assert!(
        !json.contains("model_name"),
        "model_name should be omitted when None: {json}"
    );
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
}

/// SF-2: `SF2ProviderRateLimit` round-trips through serde with the
/// retry-after value and quota classification preserved.
#[test]
fn sf2_provider_rate_limit_serde_roundtrip() {
    let original = EventV2::SF2ProviderRateLimit {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "anthropic-direct".to_owned(),
        provider_name: "anthropic".to_owned(),
        model_name: Some("claude-opus-4-7".to_owned()),
        trigger_context: "completion".to_owned(),
        recovery_attempted: true,
        retry_after_secs: 60,
        quota_kind: "rpm".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(json.contains("\"type\":\"sf2_provider_rate_limit\""));
}

/// SF-3: `SF3SchemaDecodeFailure` round-trips through serde with the
/// decode error and response size preserved.
#[test]
fn sf3_schema_decode_failure_serde_roundtrip() {
    let original = EventV2::SF3SchemaDecodeFailure {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "openai-direct".to_owned(),
        provider_name: "openai".to_owned(),
        model_name: Some("gpt-4o-2026-05-01".to_owned()),
        trigger_context: "tool_use".to_owned(),
        recovery_attempted: false,
        decode_error: "missing field `arguments` at line 1 column 245".to_owned(),
        response_bytes: 4_096,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(json.contains("\"type\":\"sf3_schema_decode_failure\""));
}

/// SF-4: `SF4ToolCallExecutionFailure` round-trips through serde with
/// the tool name, exit code, and stderr tail preserved.
#[test]
fn sf4_tool_call_execution_failure_serde_roundtrip() {
    let original = EventV2::SF4ToolCallExecutionFailure {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "anthropic-direct".to_owned(),
        provider_name: "anthropic".to_owned(),
        model_name: Some("claude-opus-4-7".to_owned()),
        trigger_context: "tool_use".to_owned(),
        recovery_attempted: false,
        tool_name: "bash".to_owned(),
        exit_code: 127,
        stderr_tail: "bash: foo: command not found\n".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(json.contains("\"type\":\"sf4_tool_call_execution_failure\""));
}

/// SF-5: `SF5ContextOverflow` round-trips through serde with the
/// token estimates and truncation flag preserved.
#[test]
fn sf5_context_overflow_serde_roundtrip() {
    let original = EventV2::SF5ContextOverflow {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "anthropic-direct".to_owned(),
        provider_name: "anthropic".to_owned(),
        model_name: Some("claude-opus-4-7".to_owned()),
        trigger_context: "tool_result_followup".to_owned(),
        recovery_attempted: true,
        input_tokens_estimated: 220_000,
        max_context_tokens: 200_000,
        truncation_applied: true,
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(json.contains("\"type\":\"sf5_context_overflow\""));
}

/// SF-6: `SF6SupervisionSetupFailed` round-trips through serde with
/// the adapter, hook name, and underlying error preserved. Distinct
/// from SF-1..5 — SF-6 is a cosmon-lab supervision-setup failure
/// emitted by `cs tackle` *after* the worker has already produced a
/// real artefact on disk; it does not carry the LLM-provider forensic
/// fields (no `provider_name`, no `model_name`, no `trigger_context`).
#[test]
fn sf6_supervision_setup_failed_serde_roundtrip() {
    let original = EventV2::SF6SupervisionSetupFailed {
        mol_id: mol(),
        worker_id: wkr(),
        adapter_name: "anthropic".to_owned(),
        hook_name: "pane_died".to_owned(),
        error: "tmux: server not running on socket".to_owned(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let back: EventV2 = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original);
    assert!(
        json.contains("\"type\":\"sf6_supervision_setup_failed\""),
        "expected snake_case discriminator: {json}"
    );
    for field in ["adapter_name", "hook_name", "error"] {
        assert!(
            json.contains(field),
            "SF-6 must carry `{field}` on the wire: {json}"
        );
    }
}

/// Cat-test for the SF *LLM-call* sub-taxonomy (SF-1..SF-5) — every
/// variant in the LLM-call sub-class carries the five cross-variant
/// forensic fields (`provider_name`, `adapter_name`, `model_name`,
/// `trigger_context`, `recovery_attempted`) on the wire. This is the
/// invariant a `select(.type | startswith("sf") and .provider_name)`
/// audit query depends on for cross-variant aggregation of LLM-call
/// silent failures.
///
/// SF-6 is intentionally excluded: it is a cosmon-lab
/// supervision-setup failure (`cs tackle` post-spawn), not an LLM
/// provider call failure, and therefore has no `provider_name` /
/// `model_name` / `trigger_context`. Its own round-trip is covered by
/// [`sf6_supervision_setup_failed_serde_roundtrip`].
#[test]
fn sf_variants_carry_shared_forensic_fields() {
    let sf_events: Vec<EventV2> = vec![
        EventV2::SF1HttpTransport {
            mol_id: mol(),
            worker_id: wkr(),
            adapter_name: "openai-direct".to_owned(),
            provider_name: "openai".to_owned(),
            model_name: Some("gpt-4o".to_owned()),
            trigger_context: "completion".to_owned(),
            recovery_attempted: false,
            retry_count: 0,
            error_class: "dns".to_owned(),
        },
        EventV2::SF2ProviderRateLimit {
            mol_id: mol(),
            worker_id: wkr(),
            adapter_name: "anthropic-direct".to_owned(),
            provider_name: "anthropic".to_owned(),
            model_name: Some("claude-opus-4-7".to_owned()),
            trigger_context: "completion".to_owned(),
            recovery_attempted: true,
            retry_after_secs: 30,
            quota_kind: "tpm".to_owned(),
        },
        EventV2::SF3SchemaDecodeFailure {
            mol_id: mol(),
            worker_id: wkr(),
            adapter_name: "openai-direct".to_owned(),
            provider_name: "openai".to_owned(),
            model_name: Some("gpt-4o".to_owned()),
            trigger_context: "tool_use".to_owned(),
            recovery_attempted: false,
            decode_error: "unknown field".to_owned(),
            response_bytes: 1024,
        },
        EventV2::SF4ToolCallExecutionFailure {
            mol_id: mol(),
            worker_id: wkr(),
            adapter_name: "anthropic-direct".to_owned(),
            provider_name: "anthropic".to_owned(),
            model_name: Some("claude-opus-4-7".to_owned()),
            trigger_context: "tool_use".to_owned(),
            recovery_attempted: false,
            tool_name: "bash".to_owned(),
            exit_code: 1,
            stderr_tail: "oops".to_owned(),
        },
        EventV2::SF5ContextOverflow {
            mol_id: mol(),
            worker_id: wkr(),
            adapter_name: "anthropic-direct".to_owned(),
            provider_name: "anthropic".to_owned(),
            model_name: Some("claude-opus-4-7".to_owned()),
            trigger_context: "completion".to_owned(),
            recovery_attempted: true,
            input_tokens_estimated: 210_000,
            max_context_tokens: 200_000,
            truncation_applied: true,
        },
    ];
    for event in sf_events {
        let json = serde_json::to_string(&event).unwrap();
        for field in [
            "provider_name",
            "adapter_name",
            "model_name",
            "trigger_context",
            "recovery_attempted",
        ] {
            assert!(
                json.contains(field),
                "SF variant must carry `{field}` on the wire: {json}"
            );
        }
    }
}

/// Karpathy cat-test (§14 badge of the briefing) — the JSON of every
/// Worker-Spawn Port variant uses the `type` discriminator the audit
/// query relies on (`select(.event | startswith("WorkerSpawn") or
/// startswith("Adapter"))`). The full audit query is exercised in
/// `cosmon-state::events::worker_spawn::tests`; this test pins the
/// schema-level invariant the query depends on.
#[test]
fn worker_spawn_port_variants_use_snake_case_type_discriminator() {
    let variants: Vec<(&str, EventV2)> = vec![
        (
            "worker_spawn_attempted",
            EventV2::WorkerSpawnAttempted {
                mol_id: mol(),
                worker_id: wkr(),
                adapter_name: "claude".to_owned(),
                worktree_path: String::new(),
                invocation_uuid: String::new(),
                pid: 0,
                pre_existing_worker: None,
            },
        ),
        (
            "adapter_liveness_probed",
            EventV2::AdapterLivenessProbed {
                mol_id: mol(),
                worker_id: wkr(),
                adapter_name: "claude".to_owned(),
                probe_kind: AdapterProbeKind::PaneSignature,
                probe_result: AdapterProbeResult::Alive {
                    evidence: String::new(),
                },
                elapsed_since_last_advance_ms: 0,
            },
        ),
        (
            "adapter_pane_signature_checked",
            EventV2::AdapterPaneSignatureChecked {
                mol_id: mol(),
                worker_id: wkr(),
                adapter_name: "claude".to_owned(),
                registered_signature: Vec::new(),
                observed_command: String::new(),
                matched: false,
                channel: PerturbationChannel::Propulsion,
            },
        ),
        (
            "adapter_briefing_consumed",
            EventV2::AdapterBriefingConsumed {
                mol_id: mol(),
                worker_id: wkr(),
                adapter_name: "claude".to_owned(),
                briefing_path: String::new(),
                briefing_seal_observed: String::new(),
                briefing_seal_recorded: String::new(),
                bytes_read: 0,
                consumed_at: Utc::now(),
            },
        ),
        (
            "adapter_handle_reconciled",
            EventV2::AdapterHandleReconciled {
                mol_id: mol(),
                worker_id: wkr(),
                adapter_name: "claude".to_owned(),
                handle_state: AdapterHandleState::Held,
                underlying_exit_observed_at: None,
                handle_released_at: Utc::now(),
                gap_ms: 0,
            },
        ),
    ];
    for (expected_tag, event) in variants {
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.get("type").and_then(|v| v.as_str()),
            Some(expected_tag),
            "variant must serialize with type={expected_tag}: {json}"
        );
    }
}
