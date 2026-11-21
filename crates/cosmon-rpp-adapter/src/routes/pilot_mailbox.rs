// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot messages of a bound attachment (issue #212, W7b): S-SEND, S-PULL
//! and S-ACK of `docs/specs/cross-machine-collaboration.md`.
//!
//! Admission is the W7a door ([`super::pilot::door`]): the mission in the
//! path must be the binding's, and the attachment is the one the caller
//! proved. The sender written into a message is that attachment's mailbox
//! ([`mailbox_of`]); no request field can name another author or mission.
//! A recipient is accepted only when it is a pilot of the mission: an
//! attachment with an active pilot binding of the mission, or a local
//! session whose presence snapshot names the mission. The `pilot-mailbox`
//! is the only writer. No lifecycle executor, lease ledger, presence writer
//! or checkpoint store is reachable from this module.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, Utc};
use cosmon_core::collaboration::{BindingSet, CollaborationScope};
use cosmon_core::id::SessionId;
use cosmon_core::pilot_message::{MessageId, PilotMessage};
use cosmon_core::remote_mailbox::{
    cut_page, mailbox_of, RemoteMailboxError, PULL_ITEM_BUDGET, PULL_PAGE,
};
use cosmon_core::remote_presence::live_attachments;
use cosmon_core::work_message::MessageKey;
use cosmon_filestore::{MailboxEntry, PilotMailbox, RemoteMailboxStoreError, RemoteSend};
use serde::Deserialize;
use serde_json::{json, Value};

use super::pilot::{admission_error, api, commit, door, envelope, invalid, Door};
use crate::error::ApiError;
use crate::AppState;

/// S-SEND request body. Author, mission and sequence are not fields: the
/// binding fixes the first two and the mailbox assigns the third.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    idempotency_key: MessageKey,
    to: String,
    message: String,
    expires_in_secs: Option<u64>,
}

/// S-PULL query: a cursor past the last sequence of the previous page.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PullQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

fn unavailable() -> ApiError {
    api(StatusCode::SERVICE_UNAVAILABLE, "custody_unavailable")
}

fn corrupt() -> ApiError {
    api(StatusCode::INTERNAL_SERVER_ERROR, "corrupt_evidence")
}

fn refusal(error: &RemoteMailboxError) -> ApiError {
    let status = match error.http_status() {
        400 => StatusCode::BAD_REQUEST,
        403 => StatusCode::FORBIDDEN,
        404 => StatusCode::NOT_FOUND,
        409 => StatusCode::CONFLICT,
        413 => StatusCode::PAYLOAD_TOO_LARGE,
        429 => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api(status, error.code())
}

fn store_error(error: RemoteMailboxStoreError) -> ApiError {
    match error {
        RemoteMailboxStoreError::Refused(error) => refusal(&error),
        RemoteMailboxStoreError::Unavailable(_) => unavailable(),
    }
}

/// A body over the transport limit is too large, not malformed.
fn body_rejection(rejection: &JsonRejection) -> ApiError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return refusal(&RemoteMailboxError::PayloadTooLarge);
    }
    invalid()
}

/// The caller's own mailbox.
fn own_mailbox(door: &Door) -> Result<SessionId, ApiError> {
    mailbox_of(door.admitted.attachment()).map_err(|_| corrupt())
}

/// Mailboxes of the mission's pilots: attachments with an active pilot
/// binding of the mission, and local sessions whose presence snapshot
/// names it.
fn recipients(door: &Door, bindings: &BindingSet) -> Result<BTreeSet<SessionId>, ApiError> {
    let mut out = BTreeSet::new();
    for attachment in live_attachments(bindings, &door.mission) {
        out.insert(mailbox_of(&attachment).map_err(|_| corrupt())?);
    }
    for presence in door.store.scan().map_err(|_| unavailable())? {
        if presence.mission.as_ref() == Some(&door.mission) {
            out.insert(presence.session_id);
        }
    }
    Ok(out)
}

/// Wire projection of one envelope, body excluded.
fn view(message: &PilotMessage) -> Value {
    json!({
        "id": message.id,
        "from": message.from,
        "to": message.to,
        "sequence": message.sequence,
        "payload_hash": message.payload_hash,
        "created_at": message.created_at,
        "expires_at": message.expires_at,
    })
}

/// S-SEND: deliver a message from the bound attachment to a pilot of its
/// mission, idempotent by key.
pub(crate) async fn send(
    State(state): State<Arc<AppState>>,
    Path(mission): Path<String>,
    headers: HeaderMap,
    body: Result<Json<SendBody>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(
        &state,
        &headers,
        &mission,
        CollaborationScope::SessionsWrite,
    )?;
    let Json(body) = body.map_err(|rejection| body_rejection(&rejection))?;
    let to = SessionId::new(body.to).map_err(|_| invalid())?;
    let sender = own_mailbox(&door)?;
    commit(&door, || {
        // Read under the shared binding lock `commit` holds, so the
        // recipient set is the one the write commits against.
        let bindings = door
            .bindings
            .load()
            .map_err(|error| admission_error(&error.into()))?;
        let recipients = recipients(&door, &bindings)?;
        let (message, written) = PilotMailbox::new(&door.state_dir)
            .send_remote(
                &door.mission,
                &sender,
                &recipients,
                &RemoteSend {
                    key: body.idempotency_key,
                    to,
                    body: body.message,
                    ttl_secs: body.expires_in_secs,
                },
                Utc::now(),
            )
            .map_err(store_error)?;
        let mut output = envelope(&door);
        output["message"] = view(&message);
        output["duplicate"] = json!(!written);
        Ok(Json(output))
    })
}

/// One listed entry of a pull: a message the wire carries, or the metadata
/// of one it cannot carry.
enum Pulled {
    Message(Value),
    Undeliverable(Value),
}

fn pulled(mailbox: &PilotMailbox, entry: &MailboxEntry, now: DateTime<Utc>) -> (Pulled, usize) {
    let mut value = view(&entry.message);
    value["state"] = json!(entry.message.state(now, entry.read_at).as_str());
    let reason = match mailbox.body(&entry.message).map(String::from_utf8) {
        None => "payload_unreadable",
        Some(Err(_)) => "payload_not_utf8",
        Some(Ok(text)) => {
            let mut message = value.clone();
            message["body"] = json!(text);
            let size = serde_json::to_vec(&message).map_or(usize::MAX, |bytes| bytes.len() + 1);
            if size <= PULL_ITEM_BUDGET {
                return (Pulled::Message(message), size);
            }
            "payload_too_large"
        }
    };
    value["reason"] = json!(reason);
    let size = serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len() + 1);
    (Pulled::Undeliverable(value), size)
}

/// S-PULL: one page of the bound attachment's unacknowledged messages, read
/// without consuming them. Takes no lock and creates no file.
pub(crate) async fn pull(
    State(state): State<Arc<AppState>>,
    Path(mission): Path<String>,
    headers: HeaderMap,
    query: Result<Query<PullQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let door = door(&state, &headers, &mission, CollaborationScope::SessionsRead)?;
    let Query(query) = query.map_err(|_| invalid())?;
    let limit = query.limit.unwrap_or(PULL_PAGE);
    if limit == 0 || limit > PULL_PAGE {
        return Err(invalid());
    }
    let after = query.after.unwrap_or(0);
    let reader = own_mailbox(&door)?;
    let mailbox = PilotMailbox::new(&door.state_dir);
    let now = Utc::now();
    let entries = mailbox.pending(&reader, now).map_err(|_| unavailable())?;
    let items = entries
        .iter()
        .filter(|entry| entry.message.sequence > after)
        .map(|entry| {
            let (item, size) = pulled(&mailbox, entry, now);
            (entry.message.sequence, item, size)
        });
    let (page, next) = cut_page(items, limit, PULL_ITEM_BUDGET);
    let mut messages = Vec::new();
    let mut undeliverable = Vec::new();
    for item in page {
        match item {
            Pulled::Message(value) => messages.push(value),
            Pulled::Undeliverable(value) => undeliverable.push(value),
        }
    }
    let mut output = envelope(&door);
    output["messages"] = json!(messages);
    output["undeliverable"] = json!(undeliverable);
    output["next_cursor"] = json!(next);
    Ok(Json(output))
}

/// S-ACK: acknowledge one message of the bound attachment's mailbox;
/// acknowledging twice succeeds without writing.
pub(crate) async fn ack(
    State(state): State<Arc<AppState>>,
    Path((mission, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let door = door(
        &state,
        &headers,
        &mission,
        CollaborationScope::SessionsWrite,
    )?;
    let id = MessageId::new(key).map_err(|_| invalid())?;
    let reader = own_mailbox(&door)?;
    commit(&door, || {
        let written = PilotMailbox::new(&door.state_dir)
            .ack_remote(&door.mission, &reader, &id, Utc::now())
            .map_err(store_error)?;
        let mut output = envelope(&door);
        output["id"] = json!(id);
        output["duplicate"] = json!(!written);
        Ok(Json(output))
    })
}
