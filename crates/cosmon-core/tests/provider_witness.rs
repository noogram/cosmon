// SPDX-License-Identifier: AGPL-3.0-only

//! Replay anonymous observations from a versioned local provider witness.
//! This checks the reducer contract; it does not launch providers or renew the
//! recorded live qualification.

use cosmon_core::presence::SessionState;
use cosmon_core::provider_lifecycle::{
    DeliveryDisposition, LifecycleReading, LifecycleSource, ProviderEventSequence,
    ProviderLifecycleEvent, ProviderLifecycleFold,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Witness {
    codex: Codex,
}

#[derive(Deserialize)]
struct Codex {
    lifecycle: Lifecycle,
}

#[derive(Deserialize)]
struct Lifecycle {
    codex_version: String,
    completion: String,
    events: Vec<Observation>,
}

#[derive(Deserialize)]
struct Observation {
    sequence: u64,
    method: String,
    reading: Reading,
}

#[derive(Deserialize)]
struct Reading {
    source: LifecycleSource,
    state: Option<SessionState>,
}

#[test]
fn codex_0_163_observations_replay_the_recorded_lifecycle() -> Result<(), Box<dyn std::error::Error>>
{
    let witness: Witness =
        serde_json::from_str(include_str!("fixtures/provider-witness-2026-10-18.json"))?;
    let lifecycle = witness.codex.lifecycle;
    assert_eq!(lifecycle.codex_version, "codex-cli 0.163.0");
    assert_eq!(lifecycle.completion, "completed");
    assert_eq!(lifecycle.events.len(), 4);

    let mut fold = ProviderLifecycleFold::default();
    for observation in lifecycle.events {
        let event = match observation.method.as_str() {
            "turn/started" => ProviderLifecycleEvent::TurnStarted,
            "item/commandExecution/requestApproval" => ProviderLifecycleEvent::ApprovalRequested,
            "turn/completed" => ProviderLifecycleEvent::TurnCompleted,
            "transport/disconnected" => ProviderLifecycleEvent::Disconnected,
            other => return Err(format!("unadmitted witness method: {other}").into()),
        };
        assert_eq!(
            fold.apply(ProviderEventSequence::new(observation.sequence), event),
            DeliveryDisposition::Applied
        );
        assert_eq!(
            fold.reading(),
            LifecycleReading {
                source: observation.reading.source,
                state: observation.reading.state,
            }
        );
    }
    assert_eq!(fold.reading(), LifecycleReading::default());
    Ok(())
}

/// A native export observed in Claude tools remains metadata: the marked
/// launch, not the callback's native payload, names the caller and guard.
#[test]
fn claude_2_1_303_tool_witness_replays_the_shared_launch_identity(
) -> Result<(), Box<dyn std::error::Error>> {
    use cosmon_core::caller_identity::{resolve, Caller};
    use serde_json::Value;

    let witness: Value =
        serde_json::from_str(include_str!("fixtures/claude-tool-witness-2026-10-19.json"))?;
    assert_eq!(witness["provider_version"], "2.1.303");
    assert_eq!(witness["distinct_launches"], true);
    assert_eq!(witness["previous_holder_unchanged"], true);
    assert_eq!(witness["presence_rows"], 3);
    let mut callers = Vec::new();
    for label in ["A", "B"] {
        let session = &witness["sessions"][label];
        assert_eq!(session["models"][0], "claude-sonnet-5-5");
        assert_eq!(session["tool_use"].as_array().map(Vec::len), Some(1));
        assert_eq!(session["tool_use"][0]["name"], "Bash");
        assert_eq!(session["tool_use"][0]["exact_command"], true);
        assert_eq!(session["barrier"]["both_hooks_overlapped"], true);
        assert_eq!(session["callback_native_matches_tool_if_present"], true);
        assert_eq!(session["callback_and_tool_launch_agree"], true);
        assert_eq!(session["presence_retains_callback_native"], true);
        // The bounded run proved identity before reaching its turn limit.
        assert_eq!(session["exit"], 1);
        assert_eq!(session["result"]["subtype"], "error_max_turns");

        let launch = format!("launch-fixture-{label}");
        let native = format!("native-fixture-{label}");
        for (stage, caller) in [
            ("hook", Caller::ClaudeCallback(Some(native.as_str()))),
            ("tool", Caller::Shell),
        ] {
            let observation = &session[stage];
            for field in [
                "fresh_fallback",
                "marker_matches",
                "presence_matches_caller",
                "check_names_same_caller",
                "check_refused_without_lease",
                "stale_parent_absent",
            ] {
                assert_eq!(observation[field], true, "{label}/{stage}/{field}");
            }
            let present = &observation["variables_present"];
            assert_eq!(present["CLAUDE_CODE_SESSION_ID"], true);
            for key in ["COSMON_SESSION_ID", "COSMON_SESSION_LAUNCH_ID"] {
                assert_eq!(present[key], true);
            }
            for key in [
                "CLAUDE_SESSION_ID",
                "CODEX_THREAD_ID",
                "CODEX_SESSION_ID",
                "COSMON_PARENT_MOL_ID",
                "COSMON_WORKER_ID",
            ] {
                assert_eq!(present[key], false);
            }
            let resolved = resolve(None, caller, |key| {
                if present[key] != true {
                    return None;
                }
                match key {
                    "COSMON_SESSION_ID" | "COSMON_SESSION_LAUNCH_ID" => Some(launch.clone()),
                    "CLAUDE_CODE_SESSION_ID" => Some(native.clone()),
                    _ => None,
                }
            })?;
            assert_eq!(
                resolved.as_ref().map(|sid| sid.as_str()),
                Some(launch.as_str())
            );
            if stage == "tool" {
                callers.push(resolved);
            }
        }
    }
    assert_ne!(callers[0], callers[1]);
    Ok(())
}
