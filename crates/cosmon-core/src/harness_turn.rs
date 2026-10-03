// SPDX-License-Identifier: AGPL-3.0-only

//! Durable turn and effect evidence for the in-process direct arms.
//!
//! The harness loop holds its conversation in memory, so a killed process used
//! to take with it everything the model had said and every tool it had run.
//! This module defines what survives: a small set of typed records, written in
//! an order that makes intent precede effect and receipt precede the next
//! step, plus the I/O-free rules for reading them back.
//!
//! Three things live here and nothing else:
//!
//! - [`TurnRecord`], the record vocabulary. A record names a turn, a call, a
//!   count or a digest; it never carries raw model or tool text. Raw content
//!   lives in immutable blobs the record points at through [`BlobRef`].
//! - [`TurnEvidenceStore`], the port a backend implements. The core defines
//!   it and performs no I/O; the state crate implements it over the fleet
//!   ledger and the molecule directory.
//! - [`reconstruct`], a pure fold of one attempt's records that reports what
//!   was spent, what was completed, and which effects are unresolved.
//!
//! The ledger owns ordering and lifecycle. A blob has a digest and a schema
//! version and no status of its own, so the evidence adds no second lifecycle
//! log: the molecule journal still projects the one ledger.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version stamped on every [`HarnessTurnEvidence`] row and blob ref.
///
/// A reader that meets a larger version must refuse the attempt instead of
/// guessing at fields it does not know.
pub const HARNESS_TURN_SCHEMA_VERSION: u32 = 1;

/// Longest free-text reason a record may carry. Longer text is truncated at a
/// character boundary so a ledger row stays small whatever a provider returns.
pub const MAX_REASON_CHARS: usize = 512;

/// SHA-256 digest of a byte string, spelled `sha256:<lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobDigest(String);

impl BlobDigest {
    const PREFIX: &'static str = "sha256:";

    /// Digest `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let hash = Sha256::digest(bytes);
        let mut text = String::with_capacity(Self::PREFIX.len() + hash.len() * 2);
        text.push_str(Self::PREFIX);
        for byte in hash {
            text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            text.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        Self(text)
    }

    /// Parse a stored digest, rejecting anything that is not `sha256:` plus
    /// 64 lowercase hex digits. A digest names a file, so it must never carry
    /// a path separator.
    ///
    /// # Errors
    /// Returns [`EvidenceError::Corrupt`] for a malformed digest.
    pub fn parse(text: &str) -> Result<Self, EvidenceError> {
        let hex = text.strip_prefix(Self::PREFIX).ok_or_else(|| {
            EvidenceError::Corrupt(format!("digest without sha256 prefix: {text}"))
        })?;
        if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(text.to_owned()))
        } else {
            Err(EvidenceError::Corrupt(format!("malformed digest: {text}")))
        }
    }

    /// The full `sha256:<hex>` spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The bare hex digits, usable as a file name.
    #[must_use]
    pub fn hex(&self) -> &str {
        self.0.strip_prefix(Self::PREFIX).unwrap_or(&self.0)
    }
}

impl fmt::Display for BlobDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a blob holds. The kind is descriptive; it does not change how the blob
/// is stored or validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobKind {
    /// The provider-native assistant envelope, tool calls included.
    AssistantEnvelope,
    /// One tool result exactly as it was appended to the log.
    ToolResult,
    /// The provider-native message log at a complete tool-result boundary.
    LogCheckpoint,
    /// Assistant text accumulated before a terminal response.
    PartialText,
}

/// A pointer to an immutable blob: what it is, how long it is, and the digest
/// its bytes must still hash to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlobRef {
    /// What the blob holds.
    pub kind: BlobKind,
    /// Digest of the blob bytes.
    pub digest: BlobDigest,
    /// Length of the blob in bytes.
    pub len: u64,
    /// Schema version of the encoding, so a reader can refuse what it cannot
    /// decode.
    pub schema_version: u32,
}

/// Why evidence could not be written or read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceError {
    /// The backing store failed. The record or blob is not durable.
    #[error("evidence store I/O: {0}")]
    Io(String),
    /// Stored evidence does not match what its reference promises.
    #[error("evidence corrupt: {0}")]
    Corrupt(String),
    /// The store refused the record, for example a blob over its size cap.
    #[error("evidence rejected: {0}")]
    Rejected(String),
}

/// How a tool result classified, as the loop saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnToolOutcome {
    /// The tool reported success. This is not proof the task succeeded.
    Succeeded,
    /// The tool reported a definite failure.
    Failed,
    /// The tool may have had effects, but its completion is not known.
    Uncertain,
}

/// Why a terminal response ended the loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalKind {
    /// An ordinary completed response.
    Normal,
    /// The provider stopped at an output limit.
    OutputLimit,
    /// The provider filtered or refused the response.
    Refused,
    /// The response ended without a valid structural terminator.
    Incomplete,
    /// A termination reason cosmon does not recognize, kept verbatim.
    Unknown {
        /// The native reason string.
        reason: String,
    },
}

/// The ceilings an attempt started under. They are recorded so a later reader
/// can see that budgets are spent, not refreshed, across a restart.
// The `max_` prefix is the vocabulary of the budgets these mirror.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLimits {
    /// Maximum provider round trips.
    pub max_turns: u32,
    /// Maximum dispatched tool calls.
    pub max_tool_calls: u32,
    /// Maximum estimated input tokens per request.
    pub max_input_tokens: u32,
}

/// One work-turn input block selected for a request: its store key and the
/// digest of the content that was sent. The content itself stays with the
/// input's own store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEvidence {
    /// Store-owned key of the block.
    pub key: String,
    /// Digest of the rendered block.
    pub digest: BlobDigest,
}

/// One tool call named by an assistant envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEvidence {
    /// Provider-assigned call identifier; pairs the call with its result.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
}

/// One durable fact about an attempt. Counts are cumulative at the moment of
/// the record, so the last record of an attempt states what was spent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum TurnRecord {
    /// The attempt began. Written once, before the first request.
    AttemptStarted {
        /// Ceilings in force.
        limits: TurnLimits,
        /// Named digests that pin what the attempt ran against, such as the
        /// tool registry, the briefing and the requested model.
        pins: BTreeMap<String, String>,
    },
    /// A request is about to be sent. Written before network I/O, so a
    /// request with no later outcome may have been billed.
    RequestIntent {
        /// Zero-based turn index.
        turn: u32,
        /// Estimated input tokens of the log at this point.
        estimated_input_tokens: u32,
        /// Tool calls dispatched so far.
        tools_spent: u32,
    },
    /// Work-turn input blocks selected for the request of `turn`.
    InputsSelected {
        /// Turn the blocks ride on.
        turn: u32,
        /// Key and digest of each selected block.
        inputs: Vec<InputEvidence>,
    },
    /// The request produced no usable response.
    RequestFailed {
        /// Turn whose request failed.
        turn: u32,
        /// Bounded failure class text; never a response body.
        reason: String,
    },
    /// The log was compacted before the request of `turn`.
    Compacted {
        /// Turn the compaction preceded.
        turn: u32,
        /// Estimated tokens before.
        tokens_before: u32,
        /// Estimated tokens after.
        tokens_after: u32,
        /// Messages replaced by the summary.
        messages_removed: u32,
    },
    /// A valid assistant envelope with tool calls was received. Written
    /// before any of its tools run.
    AssistantReceived {
        /// Turn that produced it.
        turn: u32,
        /// The calls it names, in order.
        calls: Vec<CallEvidence>,
        /// The native envelope, or `None` when the provider cannot encode it.
        envelope: Option<BlobRef>,
        /// Tool calls dispatched so far.
        tools_spent: u32,
    },
    /// A tool is about to run. Written before the effect, so an intent with
    /// no receipt is an effect of unknown outcome.
    ToolIntent {
        /// Turn the call belongs to.
        turn: u32,
        /// Call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Digest of the argument text the tool received.
        arguments_digest: BlobDigest,
        /// Tool calls dispatched, this one included.
        tools_spent: u32,
    },
    /// A tool returned and its result was appended to the log. Written before
    /// the loop advances.
    ToolReceipt {
        /// Turn the call belongs to.
        turn: u32,
        /// Call identifier.
        call_id: String,
        /// Classification of the result.
        outcome: TurnToolOutcome,
        /// The result as appended, or `None` when the provider cannot encode it.
        result: Option<BlobRef>,
        /// Tool calls dispatched so far.
        tools_spent: u32,
    },
    /// Every call of `turn` has a receipt: a complete tool-result boundary.
    Checkpoint {
        /// Turn that completed.
        turn: u32,
        /// The native log, or `None` when the provider cannot encode it.
        log: Option<BlobRef>,
        /// Tool calls dispatched so far.
        tools_spent: u32,
    },
    /// The loop ended on a terminal response.
    Terminal {
        /// Turn that produced it.
        turn: u32,
        /// Why it ended.
        disposition: TerminalKind,
        /// Text received before termination, possibly partial.
        partial_text: Option<BlobRef>,
        /// Tool calls dispatched in total.
        tools_spent: u32,
    },
}

/// A [`TurnRecord`] with the identity that scopes it: one worker attempt,
/// named by the same history id its usage records carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessTurnEvidence {
    /// Schema version of this row.
    pub schema_version: u32,
    /// History id of the worker attempt; shared with its `UsageObserved` rows.
    pub history_id: String,
    /// Worker that ran the attempt.
    pub worker_id: crate::id::WorkerId,
    /// The record.
    pub record: TurnRecord,
}

/// Where a backend persists evidence. Both methods are durable on success and
/// must not report success otherwise: the loop treats a failure as a reason to
/// stop before the next side effect.
pub trait TurnEvidenceStore: Send + Sync {
    /// Store `bytes` as an immutable blob and return its reference.
    ///
    /// # Errors
    /// Returns an [`EvidenceError`] if the blob is not durable.
    fn put_blob(&self, kind: BlobKind, bytes: &[u8]) -> Result<BlobRef, EvidenceError>;

    /// Append one record to the ordered ledger stream.
    ///
    /// # Errors
    /// Returns an [`EvidenceError`] if the record is not durable.
    fn append(&self, record: TurnRecord) -> Result<(), EvidenceError>;
}

/// Truncate free text to [`MAX_REASON_CHARS`] characters.
#[must_use]
pub fn bounded_reason(text: &str) -> String {
    text.chars().take(MAX_REASON_CHARS).collect()
}

/// A call whose tool ran to a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedCall {
    /// Turn the call belongs to.
    pub turn: u32,
    /// Call identifier.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// Classification of the result.
    pub outcome: TurnToolOutcome,
    /// The stored result, when one was encodable.
    pub result: Option<BlobRef>,
}

/// A call whose intent is recorded and whose receipt is not: the effect may or
/// may not have happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCall {
    /// Turn the call belongs to.
    pub turn: u32,
    /// Call identifier.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
}

/// The assistant envelope received at one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedAssistant {
    /// Turn that produced it.
    pub turn: u32,
    /// The calls it names.
    pub calls: Vec<CallEvidence>,
    /// The stored envelope, when one was encodable.
    pub envelope: Option<BlobRef>,
}

/// The last complete tool-result boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRef {
    /// Turn that completed.
    pub turn: u32,
    /// The stored native log, when one was encodable.
    pub log: Option<BlobRef>,
    /// Tool calls dispatched at that point.
    pub tools_spent: u32,
}

/// How an attempt ended, when it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEvidence {
    /// Turn that produced the terminal response.
    pub turn: u32,
    /// Why it ended.
    pub disposition: TerminalKind,
    /// Partial or final text, when stored.
    pub partial_text: Option<BlobRef>,
}

/// One compaction the attempt performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionEvidence {
    /// Turn the compaction preceded.
    pub turn: u32,
    /// Estimated tokens before.
    pub tokens_before: u32,
    /// Estimated tokens after.
    pub tokens_after: u32,
    /// Messages replaced by the summary.
    pub messages_removed: u32,
}

/// What one attempt's records establish.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptReconstruction {
    /// Ceilings the attempt started under.
    pub limits: Option<TurnLimits>,
    /// Pins recorded at the start.
    pub pins: BTreeMap<String, String>,
    /// Requests whose intent was written, which is the count of paid or
    /// possibly-paid turns.
    pub requests_sent: u32,
    /// Tool calls dispatched, taken from the last record that states it.
    pub tools_spent: u32,
    /// Work-turn input blocks selected across all requests, in order.
    pub inputs: Vec<InputEvidence>,
    /// Every assistant envelope received.
    pub assistants: Vec<ReceivedAssistant>,
    /// Calls that reached a receipt.
    pub completed_calls: Vec<CompletedCall>,
    /// Calls with an intent and no receipt: effects of unknown outcome.
    pub unresolved_calls: Vec<UnresolvedCall>,
    /// Turns whose request has no recorded outcome. Such a request may have
    /// been billed.
    pub unresolved_requests: Vec<u32>,
    /// Compactions performed.
    pub compactions: Vec<CompactionEvidence>,
    /// The last complete tool-result boundary.
    pub last_checkpoint: Option<CheckpointRef>,
    /// How the attempt ended, when it did.
    pub terminal: Option<TerminalEvidence>,
}

impl AttemptReconstruction {
    /// Every blob the attempt references, for digest validation.
    #[must_use]
    pub fn blob_refs(&self) -> Vec<&BlobRef> {
        let mut refs: Vec<&BlobRef> = Vec::new();
        refs.extend(self.assistants.iter().filter_map(|a| a.envelope.as_ref()));
        refs.extend(
            self.completed_calls
                .iter()
                .filter_map(|c| c.result.as_ref()),
        );
        refs.extend(self.last_checkpoint.iter().filter_map(|c| c.log.as_ref()));
        refs.extend(self.terminal.iter().filter_map(|t| t.partial_text.as_ref()));
        refs
    }
}

/// A record sequence that breaks the ordering the loop promises.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SequenceError {
    /// A record appeared before the record it depends on.
    #[error("record out of order: {0}")]
    OutOfOrder(String),
    /// A second `attempt_started` for the same attempt.
    #[error("attempt started twice")]
    StartedTwice,
}

/// Fold one attempt's records, in ledger order, into what they establish.
///
/// The fold enforces the ordering the writer guarantees: a response follows its
/// request intent, a tool intent follows the envelope that named the call, and
/// a receipt follows its intent. A sequence that breaks it is refused instead
/// of repaired, because a gap in the ledger is a fact about what is known.
///
/// # Errors
/// Returns a [`SequenceError`] for a record that violates the ordering.
pub fn reconstruct(records: &[TurnRecord]) -> Result<AttemptReconstruction, SequenceError> {
    let mut fold = Fold::default();
    for record in records {
        fold.apply(record)?;
    }
    Ok(fold.finish())
}

/// Running state of [`reconstruct`].
#[derive(Default)]
struct Fold {
    out: AttemptReconstruction,
    started: bool,
    /// Turns with an intent and no outcome yet.
    open_requests: Vec<u32>,
    /// Calls named by an envelope: (turn, call, tool).
    named: Vec<(u32, String, String)>,
    /// Calls with an intent and no receipt yet.
    intents: BTreeMap<(u32, String), String>,
}

impl Fold {
    // One arm per record kind: the ordering rules are easier to audit side by
    // side than scattered across helpers.
    #[allow(clippy::too_many_lines)]
    fn apply(&mut self, record: &TurnRecord) -> Result<(), SequenceError> {
        let Self {
            out,
            started,
            open_requests,
            named,
            intents,
        } = self;
        match record {
            TurnRecord::AttemptStarted { limits, pins } => {
                if *started {
                    return Err(SequenceError::StartedTwice);
                }
                *started = true;
                out.limits = Some(*limits);
                out.pins.clone_from(pins);
            }
            TurnRecord::RequestIntent {
                turn, tools_spent, ..
            } => {
                out.requests_sent = out.requests_sent.saturating_add(1);
                out.tools_spent = out.tools_spent.max(*tools_spent);
                open_requests.push(*turn);
            }
            TurnRecord::InputsSelected { turn, inputs } => {
                if !open_requests.contains(turn) {
                    return Err(SequenceError::OutOfOrder(format!(
                        "inputs for turn {turn} without a request intent"
                    )));
                }
                out.inputs.extend(inputs.iter().cloned());
            }
            TurnRecord::RequestFailed { turn, .. } => {
                close_request(open_requests, *turn, "request_failed")?;
            }
            TurnRecord::Compacted {
                turn,
                tokens_before,
                tokens_after,
                messages_removed,
            } => out.compactions.push(CompactionEvidence {
                turn: *turn,
                tokens_before: *tokens_before,
                tokens_after: *tokens_after,
                messages_removed: *messages_removed,
            }),
            TurnRecord::AssistantReceived {
                turn,
                calls,
                envelope,
                tools_spent,
            } => {
                close_request(open_requests, *turn, "assistant_received")?;
                out.tools_spent = out.tools_spent.max(*tools_spent);
                for call in calls {
                    named.push((*turn, call.call_id.clone(), call.tool.clone()));
                }
                out.assistants.push(ReceivedAssistant {
                    turn: *turn,
                    calls: calls.clone(),
                    envelope: envelope.clone(),
                });
            }
            TurnRecord::ToolIntent {
                turn,
                call_id,
                tool,
                tools_spent,
                ..
            } => {
                if !named.iter().any(|(t, c, _)| t == turn && c == call_id) {
                    return Err(SequenceError::OutOfOrder(format!(
                        "tool intent {call_id} not named by the envelope of turn {turn}"
                    )));
                }
                out.tools_spent = out.tools_spent.max(*tools_spent);
                intents.insert((*turn, call_id.clone()), tool.clone());
            }
            TurnRecord::ToolReceipt {
                turn,
                call_id,
                outcome,
                result,
                tools_spent,
            } => {
                let Some(tool) = intents.remove(&(*turn, call_id.clone())) else {
                    return Err(SequenceError::OutOfOrder(format!(
                        "tool receipt {call_id} without an intent at turn {turn}"
                    )));
                };
                out.tools_spent = out.tools_spent.max(*tools_spent);
                out.completed_calls.push(CompletedCall {
                    turn: *turn,
                    call_id: call_id.clone(),
                    tool,
                    outcome: *outcome,
                    result: result.clone(),
                });
            }
            TurnRecord::Checkpoint {
                turn,
                log,
                tools_spent,
            } => {
                out.tools_spent = out.tools_spent.max(*tools_spent);
                out.last_checkpoint = Some(CheckpointRef {
                    turn: *turn,
                    log: log.clone(),
                    tools_spent: *tools_spent,
                });
            }
            TurnRecord::Terminal {
                turn,
                disposition,
                partial_text,
                tools_spent,
            } => {
                close_request(open_requests, *turn, "terminal")?;
                out.tools_spent = out.tools_spent.max(*tools_spent);
                out.terminal = Some(TerminalEvidence {
                    turn: *turn,
                    disposition: disposition.clone(),
                    partial_text: partial_text.clone(),
                });
            }
        }
        Ok(())
    }

    fn finish(self) -> AttemptReconstruction {
        let mut out = self.out;
        out.unresolved_requests = self.open_requests;
        out.unresolved_calls = self
            .intents
            .into_iter()
            .map(|((turn, call_id), tool)| UnresolvedCall {
                turn,
                call_id,
                tool,
            })
            .collect();
        out
    }
}

fn close_request(open: &mut Vec<u32>, turn: u32, what: &str) -> Result<(), SequenceError> {
    match open.iter().position(|t| *t == turn) {
        Some(index) => {
            open.remove(index);
            Ok(())
        }
        None => Err(SequenceError::OutOfOrder(format!(
            "{what} at turn {turn} without a request intent"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> TurnLimits {
        TurnLimits {
            max_turns: 30,
            max_tool_calls: 64,
            max_input_tokens: 32_768,
        }
    }

    fn call(id: &str) -> CallEvidence {
        CallEvidence {
            call_id: id.to_owned(),
            tool: "write_file".to_owned(),
        }
    }

    fn started() -> TurnRecord {
        TurnRecord::AttemptStarted {
            limits: limits(),
            pins: BTreeMap::new(),
        }
    }

    fn intent(turn: u32, spent: u32) -> TurnRecord {
        TurnRecord::RequestIntent {
            turn,
            estimated_input_tokens: 10,
            tools_spent: spent,
        }
    }

    #[test]
    fn digest_is_stable_and_round_trips_through_parse() {
        let digest = BlobDigest::of(b"abc");
        assert_eq!(
            digest.as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(BlobDigest::parse(digest.as_str()), Ok(digest.clone()));
        assert_eq!(digest.hex().len(), 64);
    }

    #[test]
    fn a_digest_that_could_name_a_path_is_refused() {
        assert!(BlobDigest::parse("sha256:../../etc/passwd").is_err());
        assert!(BlobDigest::parse("md5:00").is_err());
        let upper = format!("sha256:{}", "A".repeat(64));
        assert!(BlobDigest::parse(&upper).is_err());
    }

    #[test]
    fn a_crash_between_envelope_and_receipt_leaves_the_call_unresolved() {
        let records = vec![
            started(),
            intent(0, 0),
            TurnRecord::AssistantReceived {
                turn: 0,
                calls: vec![call("c1")],
                envelope: None,
                tools_spent: 0,
            },
            TurnRecord::ToolIntent {
                turn: 0,
                call_id: "c1".to_owned(),
                tool: "write_file".to_owned(),
                arguments_digest: BlobDigest::of(b"{}"),
                tools_spent: 1,
            },
        ];
        let got = reconstruct(&records).expect("ordered");
        assert_eq!(got.requests_sent, 1);
        assert_eq!(got.tools_spent, 1);
        assert_eq!(got.unresolved_calls.len(), 1);
        assert_eq!(got.unresolved_calls[0].call_id, "c1");
        assert!(got.completed_calls.is_empty());
        assert!(got.unresolved_requests.is_empty());
    }

    #[test]
    fn a_request_with_no_outcome_is_reported_as_possibly_billed() {
        let got = reconstruct(&[started(), intent(0, 0)]).expect("ordered");
        assert_eq!(got.unresolved_requests, vec![0]);
    }

    #[test]
    fn a_receipt_without_its_intent_is_refused() {
        let records = vec![
            started(),
            intent(0, 0),
            TurnRecord::AssistantReceived {
                turn: 0,
                calls: vec![call("c1")],
                envelope: None,
                tools_spent: 0,
            },
            TurnRecord::ToolReceipt {
                turn: 0,
                call_id: "c1".to_owned(),
                outcome: TurnToolOutcome::Succeeded,
                result: None,
                tools_spent: 1,
            },
        ];
        assert!(matches!(
            reconstruct(&records),
            Err(SequenceError::OutOfOrder(_))
        ));
    }

    #[test]
    fn a_tool_intent_for_a_call_the_envelope_did_not_name_is_refused() {
        let records = vec![
            started(),
            intent(0, 0),
            TurnRecord::AssistantReceived {
                turn: 0,
                calls: vec![call("c1")],
                envelope: None,
                tools_spent: 0,
            },
            TurnRecord::ToolIntent {
                turn: 0,
                call_id: "other".to_owned(),
                tool: "write_file".to_owned(),
                arguments_digest: BlobDigest::of(b"{}"),
                tools_spent: 1,
            },
        ];
        assert!(reconstruct(&records).is_err());
    }

    #[test]
    fn a_second_start_is_refused() {
        assert_eq!(
            reconstruct(&[started(), started()]),
            Err(SequenceError::StartedTwice)
        );
    }

    #[test]
    fn spent_budgets_and_the_last_checkpoint_come_from_the_records() {
        let records = vec![
            started(),
            intent(0, 0),
            TurnRecord::AssistantReceived {
                turn: 0,
                calls: vec![call("c1")],
                envelope: None,
                tools_spent: 0,
            },
            TurnRecord::ToolIntent {
                turn: 0,
                call_id: "c1".to_owned(),
                tool: "write_file".to_owned(),
                arguments_digest: BlobDigest::of(b"{}"),
                tools_spent: 1,
            },
            TurnRecord::ToolReceipt {
                turn: 0,
                call_id: "c1".to_owned(),
                outcome: TurnToolOutcome::Succeeded,
                result: None,
                tools_spent: 1,
            },
            TurnRecord::Checkpoint {
                turn: 0,
                log: None,
                tools_spent: 1,
            },
            intent(1, 1),
            TurnRecord::Terminal {
                turn: 1,
                disposition: TerminalKind::OutputLimit,
                partial_text: None,
                tools_spent: 1,
            },
        ];
        let got = reconstruct(&records).expect("ordered");
        assert_eq!(got.requests_sent, 2);
        assert_eq!(got.tools_spent, 1);
        assert_eq!(got.last_checkpoint.map(|c| c.turn), Some(0));
        assert_eq!(
            got.terminal.map(|t| t.disposition),
            Some(TerminalKind::OutputLimit)
        );
        assert!(got.unresolved_calls.is_empty());
        assert!(got.unresolved_requests.is_empty());
    }

    #[test]
    fn records_round_trip_through_json_with_a_stable_tag() {
        let record = intent(3, 2);
        let text = serde_json::to_string(&record).expect("serialise");
        assert!(text.contains(r#""record":"request_intent""#));
        let back: TurnRecord = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, record);
    }

    #[test]
    fn reasons_are_bounded() {
        assert_eq!(
            bounded_reason(&"é".repeat(2000)).chars().count(),
            MAX_REASON_CHARS
        );
    }
}
