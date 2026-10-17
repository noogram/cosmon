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
