// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence and message commands (issue #212, W7a and W7b). The
//! attachment and its proof come from the profile and the private credential
//! store, exactly as for `work`; the host takes mission, attachment and
//! author from the binding.

use clap::Subcommand;
use cosmon_remote::canon;
use cosmon_remote::credential::CredentialStore;
use cosmon_remote::pilot::{AttachRequest, SendRequest};
use cosmon_remote::{Error, Profile, Result};

#[derive(Debug, Subcommand)]
pub(crate) enum PilotCmd {
    #[command(about = format!("Attach the bound attachment to its mission, at most two per mission ({})", canon::POST_V1_PILOT_MISSION_ATTACHMENTS.label()))]
    Attach {
        mission: String,
        /// Keep this key unchanged when retrying a lost response.
        #[arg(long)]
        key: String,
        /// Display label; never authority.
        #[arg(long)]
        label: Option<String>,
    },
    #[command(about = format!("Record a server-observed heartbeat, at most one per 10 seconds ({})", canon::PUT_V1_PILOT_MISSION_ATTACHMENTS_ATTACHMENT_HEARTBEAT.label()))]
    Heartbeat { mission: String },
    #[command(about = format!("Withdraw the bound attachment's presence; safe to repeat ({})", canon::DELETE_V1_PILOT_MISSION_ATTACHMENTS_ATTACHMENT.label()))]
    Detach { mission: String },
    #[command(about = format!("List one page of the mission's attachments ({})", canon::GET_V1_PILOT_MISSION_ATTACHMENTS.label()))]
    Peers {
        mission: String,
        /// Cursor: the last attachment of the previous page.
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    #[command(about = format!("Send a message to a pilot of the mission, at most 65536 bytes ({})", canon::POST_V1_PILOT_MISSION_MESSAGES.label()))]
    Send {
        mission: String,
        /// Recipient: an attachment or local session piloting the mission.
        #[arg(long)]
        to: String,
        /// Keep this key unchanged when retrying a lost response.
        #[arg(long)]
        key: String,
        /// Message body.
        #[arg(long)]
        message: String,
        /// Seconds after which an unacknowledged message reads as expired.
        #[arg(long = "expires-in", value_name = "SECONDS")]
        expires_in: Option<u64>,
    },
    #[command(about = format!("List unacknowledged messages without consuming them ({})", canon::GET_V1_PILOT_MISSION_MESSAGES.label()))]
    Inbox {
        mission: String,
        /// Cursor: the last sequence of the previous page.
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    #[command(about = format!("Acknowledge one message after its safe point; safe to repeat ({})", canon::POST_V1_PILOT_MISSION_MESSAGES_KEY_ACK.label()))]
    Ack { mission: String, key: String },
}

pub(crate) async fn run(
    profile: &Profile,
    cmd: PilotCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let slot = crate::work_cli::slot(profile)?;
    let proof = CredentialStore::detect()?
        .load_attachment_proof(&slot)?
        .ok_or_else(|| {
            Error::Config(
                "no attachment proof stored; import the host-issued proof with `work store-proof` through stdin"
                    .into(),
            )
        })?;
    let client = crate::client_for(profile, token).await?;
    let attachment = slot.attachment();
    let response = match cmd {
        PilotCmd::Attach {
            mission,
            key,
            label,
        } => {
            client
                .pilot_attach(
                    &mission,
                    attachment,
                    &proof,
                    &AttachRequest {
                        idempotency_key: key,
                        label,
                    },
                )
                .await?
        }
        PilotCmd::Heartbeat { mission } => {
            client.pilot_heartbeat(&mission, attachment, &proof).await?
        }
        PilotCmd::Detach { mission } => client.pilot_detach(&mission, attachment, &proof).await?,
        PilotCmd::Peers {
            mission,
            after,
            limit,
        } => {
            client
                .pilot_peers(&mission, attachment, &proof, after.as_deref(), limit)
                .await?
        }
        PilotCmd::Send {
            mission,
            to,
            key,
            message,
            expires_in,
        } => {
            client
                .pilot_send(
                    &mission,
                    attachment,
                    &proof,
                    &SendRequest {
                        idempotency_key: key,
                        to,
                        message,
                        expires_in_secs: expires_in,
                    },
                )
                .await?
        }
        PilotCmd::Inbox {
            mission,
            after,
            limit,
        } => {
            client
                .pilot_pull(&mission, attachment, &proof, after, limit)
                .await?
        }
        PilotCmd::Ack { mission, key } => {
            client.pilot_ack(&mission, &key, attachment, &proof).await?
        }
    };
    crate::print_json(json, &response);
    Ok(())
}
