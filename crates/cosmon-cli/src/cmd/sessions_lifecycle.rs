// SPDX-License-Identifier: AGPL-3.0-only

//! Ingress for lifecycle observations owned by a connected provider client.
//!
//! This command is deliberately one-shot. Pupitre or another client owns the
//! Codex app-server connection and forwards only event kind, session identity,
//! and a monotonically increasing sequence. Cosmon neither opens a resident
//! connection nor gains authority over the provider session.

use cosmon_core::id::SessionId;
use cosmon_core::presence::SessionKind;
use cosmon_core::provider_lifecycle::{
    DeliveryDisposition, LifecycleReading, ProviderEventSequence, ProviderLifecycleEvent,
    ProviderLifecycleFold,
};

use super::{presence, Context};

/// Arguments for `cs sessions lifecycle`.
#[derive(clap::Args)]
pub struct Args {
    /// Existing Cosmon presence session whose lifecycle was observed.
    #[arg(long, value_name = "SID")]
    pub session: String,
    /// Connected provider. Only `codex` app-server events are admitted today.
    #[arg(long, default_value = "codex")]
    pub provider: String,
    /// Client-assigned sequence, strictly increasing for this provider session
    /// across reconnects.
    #[arg(long, value_name = "N")]
    pub sequence: u64,
    /// Observed app-server event; no free-form content is accepted.
    #[arg(long, value_enum)]
    pub event: Event,
}

/// Content-free Codex app-server lifecycle events admitted by the adapter.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Event {
    /// `turn/started` notification.
    TurnStart,
    /// Any stable `item/*/requestApproval` server request.
    ApprovalRequest,
    /// `turn/completed` notification.
    TurnComplete,
    /// Loss of the app-server connection.
    Disconnect,
}

impl From<Event> for ProviderLifecycleEvent {
    fn from(value: Event) -> Self {
        match value {
            Event::TurnStart => Self::TurnStarted,
            Event::ApprovalRequest => Self::ApprovalRequested,
            Event::TurnComplete => Self::TurnCompleted,
            Event::Disconnect => Self::Disconnected,
        }
    }
}

/// Fold and persist one provider observation.
///
/// # Errors
///
/// Refuses unsupported providers, invalid session ids, or a session without an
/// existing presence row, and propagates presence-store failures. Duplicate
/// and stale deliveries are successful no-ops and never refresh the heartbeat
/// or overwrite a newer observation.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.provider != "codex" {
        anyhow::bail!(
            "unsupported lifecycle provider '{}' — expected 'codex'",
            args.provider
        );
    }
    let sid = SessionId::new(args.session.clone())?;
    let mut snapshot = presence::store(ctx).load(&sid)?.ok_or_else(|| {
        anyhow::anyhow!(
            "refusing lifecycle for session {}: no presence row; establish it with sessions attach or a provider hook first",
            sid.as_str()
        )
    })?;
    if let Some(provider) = snapshot.provider.as_deref() {
        if provider != "codex" {
            anyhow::bail!(
                "refusing Codex lifecycle for session {}: presence provider is '{}'",
                sid.as_str(),
                provider
            );
        }
    }
    let prior_reading = LifecycleReading {
        source: snapshot.lifecycle_source,
        state: snapshot.state,
    };
    let mut fold = ProviderLifecycleFold::restore(snapshot.lifecycle_sequence, prior_reading);
    let disposition = fold.apply(ProviderEventSequence::new(args.sequence), args.event.into());

    if disposition != DeliveryDisposition::Applied {
        render(ctx, disposition, fold.reading(), false);
        return Ok(());
    }

    let reading = fold.reading();
    let changed = reading != prior_reading;
    let worker_molecule = snapshot.current_molecule.clone();
    let kind = if worker_molecule.is_some() {
        SessionKind::Worker
    } else {
        SessionKind::Pilot
    };

    if changed {
        crate::operator_event::emit_session_presence(
            &ctx.state_dir(),
            &sid,
            Some("codex"),
            kind,
            None,
            worker_molecule.clone(),
            crate::operator_event::SessionPresenceObservation {
                state: reading.state,
                source: reading.source,
                detail: None,
            },
        );
    }

    let now = chrono::Utc::now();
    // Lifecycle ingress updates the heartbeat, state, source, and sequence;
    // clears lifecycle detail; and initializes provider only when it is absent.
    // It does not replace the source session's pid, cwd, galaxy, seat, mission,
    // provider-native identity, capabilities, headline, or worker linkage.
    snapshot.heartbeat_at = now;
    snapshot.state = reading.state;
    snapshot.lifecycle_source = reading.source;
    snapshot.lifecycle_sequence = fold.last_sequence();
    snapshot.detail = None;
    if snapshot.provider.is_none() {
        snapshot.provider = Some("codex".to_owned());
    }
    presence::store(ctx).upsert(&snapshot)?;
    render(ctx, disposition, reading, changed);
    Ok(())
}

fn render(
    ctx: &Context,
    disposition: DeliveryDisposition,
    reading: LifecycleReading,
    changed: bool,
) {
    let disposition = match disposition {
        DeliveryDisposition::Applied => "applied",
        DeliveryDisposition::Duplicate => "duplicate",
        DeliveryDisposition::Stale => "stale",
    };
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "disposition": disposition,
                "changed": changed,
                "source": reading.source,
                "state": reading.state,
            })
        );
    } else {
        println!(
            "lifecycle {disposition}: source={} state={}",
            reading.source.as_str(),
            reading.state.map_or("unavailable", |state| state.as_str())
        );
    }
}
