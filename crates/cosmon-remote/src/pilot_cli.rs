// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence commands (issue #212, W7a). The attachment and its proof
//! come from the profile and the private credential store, exactly as for
//! `work`; the host takes mission and attachment from the binding.

use clap::Subcommand;
use cosmon_remote::canon;
use cosmon_remote::credential::CredentialStore;
use cosmon_remote::pilot::AttachRequest;
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
    };
    crate::print_json(json, &response);
    Ok(())
}
