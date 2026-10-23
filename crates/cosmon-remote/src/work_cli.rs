// SPDX-License-Identifier: AGPL-3.0-only

//! Explicit advisory work commands, without a background outbox or lifecycle effects.

use clap::Subcommand;
use cosmon_core::collaboration::{AttachmentId, AttachmentProof};
use cosmon_core::work_message::{Confidentiality, DeliveryAdapter, Disposition};
use cosmon_remote::canon;
use cosmon_remote::credential::{AttachmentSlot, CredentialStore};
use cosmon_remote::work::{AckRequest, PullRequest, SendRequest};
use cosmon_remote::{Error, Profile, Result};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum WorkCmd {
    #[command(about = format!("Read the bound roster ({})", canon::GET_V1_WORK_OWNER.label()))]
    List {
        owner: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    #[command(about = format!("Admit advisory evidence ({})", canon::POST_V1_WORK_OWNER_MESSAGES.label()))]
    Send {
        owner: String,
        #[arg(long)]
        scope_revision: String,
        #[arg(long)]
        to: String,
        /// Keep this key unchanged when retrying a lost response.
        #[arg(long)]
        key: String,
        #[arg(long, conflicts_with = "file", required_unless_present = "file")]
        text: Option<String>,
        #[arg(long, conflicts_with = "text", required_unless_present = "text")]
        file: Option<PathBuf>,
        #[arg(long = "reply", alias = "reply-to")]
        reply: Option<String>,
        #[arg(long)]
        phase: Option<String>,
        #[arg(long)]
        ttl: Option<u64>,
        #[arg(long, value_parser=["internal", "confidential"], default_value="internal")]
        confidentiality: String,
    },
    #[command(about = format!("Pull at an explicit safe point ({}); --peek uses {}", canon::POST_V1_WORK_OWNER_INBOX.label(), canon::GET_V1_WORK_OWNER_INBOX.label()))]
    Inbox {
        owner: String,
        #[arg(long)]
        scope_revision: String,
        #[arg(long)]
        peek: bool,
        #[arg(long, default_value = "pull", value_parser=["pull", "claude_post_tool_use", "codex_post_tool_use", "harness_turn"])]
        adapter: String,
    },
    #[command(about = format!("Record consideration, separately from a result ({})", canon::POST_V1_WORK_OWNER_MESSAGES_KEY_ACK.label()))]
    Ack {
        owner: String,
        key: String,
        #[arg(long)]
        scope_revision: String,
        #[arg(long)]
        payload_digest: String,
        #[arg(long, required_unless_present_any=["deferred","rejected"], conflicts_with_all=["deferred","rejected"])]
        considered: bool,
        #[arg(long, conflicts_with = "rejected")]
        deferred: bool,
        #[arg(long)]
        rejected: bool,
        #[arg(long)]
        reply: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Store the host-issued proof from stdin privately; never pass it in argv.
    StoreProof,
}

fn slot(profile: &Profile) -> Result<AttachmentSlot> {
    let attachment = profile.attachment.as_deref().ok_or_else(|| {
        Error::Config("set the host-issued attachment with `config set attachment <id>`".into())
    })?;
    let attachment =
        AttachmentId::new(attachment).map_err(|_| Error::Config("invalid attachment ID".into()))?;
    Ok(AttachmentSlot::new(profile.credential_key()?, attachment))
}

#[allow(clippy::too_many_lines)] // One explicit dispatch arm per advisory operation.
pub(crate) async fn run(
    profile: &Profile,
    cmd: WorkCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let slot = slot(profile)?;
    let store = CredentialStore::detect()?;
    if matches!(cmd, WorkCmd::StoreProof) {
        let mut input = zeroize::Zeroizing::new(String::new());
        std::io::stdin().take(4097).read_to_string(&mut input)?;
        if input.len() > 4096 {
            return Err(Error::Config(
                "attachment proof exceeds its input bound".into(),
            ));
        }
        let proof = AttachmentProof::parse(input.trim())
            .map_err(|_| Error::Config("invalid attachment proof".into()))?;
        if store.store_attachment_proof(&slot, &proof)?
            != cosmon_remote::credential::StoreOutcome::Persisted
        {
            return Err(Error::Config("attachment proof requires a persistent credential backend; unset COSMON_REMOTE_TOKEN and use --token for a static bearer".into()));
        }
        crate::print_json(json, &serde_json::json!({"stored":true}));
        return Ok(());
    }
    let proof = store.load_attachment_proof(&slot)?.ok_or_else(|| Error::Config("no attachment proof stored; import the host-issued proof with `work store-proof` through stdin".into()))?;
    let client = crate::client_for(profile, token).await?;
    let response = match cmd {
        WorkCmd::List {
            owner,
            offset,
            limit,
        } => {
            client
                .work_list(&owner, slot.attachment(), &proof, offset, limit)
                .await?
        }
        WorkCmd::Send {
            owner,
            scope_revision,
            to,
            key,
            text,
            file,
            reply,
            phase,
            ttl,
            confidentiality,
        } => {
            let payload = match (text, file) {
                (Some(text), _) => text,
                (_, Some(path)) => {
                    let mut value = String::new();
                    std::fs::File::open(path)?
                        .take(1_048_577)
                        .read_to_string(&mut value)?;
                    if value.len() > 1_048_576 {
                        return Err(Error::Config("payload exceeds client input bound".into()));
                    }
                    value
                }
                _ => return Err(Error::Config("send requires --text or --file".into())),
            };
            let body = SendRequest {
                scope_revision,
                recipient: to,
                idempotency_key: key,
                sender_time: chrono::Utc::now().to_rfc3339(),
                reply_to: reply,
                phase,
                confidentiality: if confidentiality == "confidential" {
                    Confidentiality::Confidential
                } else {
                    Confidentiality::Internal
                },
                ttl_secs: ttl,
                payload,
            };
            client
                .work_send(&owner, slot.attachment(), &proof, &body)
                .await?
        }
        WorkCmd::Inbox {
            owner,
            scope_revision,
            peek,
            adapter,
        } => {
            client
                .work_inbox(
                    &owner,
                    slot.attachment(),
                    &proof,
                    &PullRequest {
                        scope_revision,
                        adapter: match adapter.as_str() {
                            "claude_post_tool_use" => DeliveryAdapter::ClaudePostToolUse,
                            "codex_post_tool_use" => DeliveryAdapter::CodexPostToolUse,
                            "harness_turn" => DeliveryAdapter::HarnessTurn,
                            _ => DeliveryAdapter::Pull,
                        },
                        context_observation: None,
                        peek,
                    },
                )
                .await?
        }
        WorkCmd::Ack {
            owner,
            key,
            scope_revision,
            payload_digest,
            considered,
            deferred,
            reply,
            note,
            ..
        } => {
            client
                .work_ack(
                    &owner,
                    &key,
                    slot.attachment(),
                    &proof,
                    &AckRequest {
                        scope_revision,
                        payload_digest,
                        disposition: if considered {
                            Disposition::Considered
                        } else if deferred {
                            Disposition::Deferred
                        } else {
                            Disposition::Rejected
                        },
                        reply,
                        note,
                    },
                )
                .await?
        }
        WorkCmd::StoreProof => {
            return Err(Error::Config("proof import was already handled".into()))
        }
    };
    crate::print_json(json, &response);
    Ok(())
}
