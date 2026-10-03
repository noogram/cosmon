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
    /// The attempt continues an earlier, interrupted one. Written once,
    /// directly after `attempt_started`, before the first request.
    ///
    /// It names the attempt it continues and the checkpoint it restored, and
    /// carries the budgets that attempt had already spent, so a chain of
    /// resumes can never refresh a counter or a deadline.
    Resumed {
        /// History id of the attempt this one continues.
        from_history_id: String,
        /// Turn of the checkpoint the log was restored from.
        checkpoint_turn: u32,
        /// The checkpoint's native log; the same blob the earlier attempt wrote.
        log: BlobRef,
        /// Tool calls already dispatched at that checkpoint.
        tools_spent: u32,
        /// Provider round trips already made across the chain.
        requests_sent: u32,
        /// Wall-clock seconds already spent across the chain.
        elapsed_secs: u64,
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
    /// The attempt this one continues, when it is a resumption.
    pub resumed_from: Option<ResumedFrom>,
}

/// What a resumed attempt inherited from the attempt it continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumedFrom {
    /// History id of the attempt that was continued.
    pub history_id: String,
    /// Turn of the restored checkpoint.
    pub checkpoint_turn: u32,
    /// Provider round trips already made across the chain.
    pub requests_sent: u32,
    /// Wall-clock seconds already spent across the chain.
    pub elapsed_secs: u64,
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

/// Name of the shell tool. Its session keeps a working directory, exports and
/// background processes that no record captures, so an attempt that used it
/// cannot be continued on the assumption that they are intact.
pub const SHELL_TOOL_NAME: &str = "exec_command";

/// Why an interrupted attempt may not be continued automatically.
///
/// Every variant is a fact about what the evidence does not establish. None is
/// a failure of the attempt, and none licenses replaying anything: the
/// remedy is reconciliation by an operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeRefusal {
    /// The attempt already ended on a terminal response.
    #[error("the attempt already ended on a terminal response; there is nothing to resume")]
    AlreadyTerminal,
    /// A tool has an intent and no receipt: its effect may have happened.
    #[error(
        "{} tool call(s) have an intent and no receipt (first: `{}` call `{}`); \
         their effect is unknown and will not be repeated or assumed",
        .calls.len(), .calls[0].tool, .calls[0].call_id
    )]
    UnresolvedEffect {
        /// The calls whose outcome is unknown.
        calls: Vec<UnresolvedCall>,
    },
    /// A request has no recorded outcome: it may have been billed.
    #[error(
        "the request of turn {} has no recorded outcome and may have been billed; \
         it will not be sent again without reconciliation",
        .turns[0]
    )]
    UnresolvedRequest {
        /// Turns whose request has no outcome.
        turns: Vec<u32>,
    },
    /// A tool of the attempt used the persistent shell.
    #[error(
        "the attempt ran `{SHELL_TOOL_NAME}`; a fresh shell does not recover its working \
         directory, exports or processes, so continuing would invent that state"
    )]
    ShellState,
    /// The attempt never reached a complete tool-result boundary.
    #[error("the attempt has no complete tool-result checkpoint to continue from")]
    NoCheckpoint,
    /// Work was begun after the last checkpoint and not completed.
    #[error(
        "turn {turn} began after the last checkpoint without completing; its tool \
         effects are not covered by a checkpoint"
    )]
    IncompleteTurn {
        /// The turn that started after the checkpoint.
        turn: u32,
    },
    /// The attempt recorded no starting ceilings.
    #[error("the attempt never recorded its ceilings")]
    NoLimits,
    /// A pinned input differs from what the attempt ran against.
    #[error("`{name}` changed since the attempt started (recorded {recorded}, now {current}); re-admit explicitly")]
    PinChanged {
        /// Pin name, such as `formula` or `requested_model`.
        name: String,
        /// Value recorded at the start of the attempt.
        recorded: String,
        /// Value the continuation would run with.
        current: String,
    },
    /// The configured ceilings differ from the recorded ones.
    #[error(
        "the loop ceilings changed since the attempt started; budgets are spent, not refreshed"
    )]
    LimitsChanged,
    /// The attempt's own wall-clock budget is already spent.
    #[error("the wall-clock deadline is already spent ({spent_secs}s of {limit_secs}s)")]
    DeadlineSpent {
        /// Seconds already spent across the chain.
        spent_secs: u64,
        /// The configured limit.
        limit_secs: u64,
    },
    /// The stored evidence is damaged or breaks the ordering the writer guarantees.
    #[error("the evidence is damaged: {0}")]
    Corrupt(String),
    /// The provider's log cannot be restored from a checkpoint.
    #[error("this provider cannot restore its message log from a checkpoint")]
    LogNotRestorable,
    /// Another attempt owns this molecule's loop.
    #[error("another in-process attempt owns this molecule's loop")]
    ActiveOwner,
    /// Resume was requested and the molecule has no interrupted attempt.
    #[error("the molecule has no interrupted in-process attempt to resume")]
    NothingToResume,
}

/// What an interrupted attempt establishes about a safe continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// History id of the attempt being continued.
    pub from_history_id: String,
    /// Ceilings the attempt ran under; the continuation must run under the same.
    pub limits: TurnLimits,
    /// Pins the attempt ran against.
    pub pins: BTreeMap<String, String>,
    /// The turn the continuation sends first.
    pub next_turn: u32,
    /// Tool calls already dispatched.
    pub tools_spent: u32,
    /// Provider round trips already made across the chain.
    pub requests_sent: u32,
    /// Wall-clock seconds already spent across the chain.
    pub elapsed_secs: u64,
    /// The checkpoint to restore the native log from.
    pub checkpoint_turn: u32,
    /// The checkpoint's native log.
    pub log: BlobRef,
}

impl ResumePlan {
    /// Require every pin the attempt recorded to equal the one the
    /// continuation would run with, and the ceilings to be unchanged.
    ///
    /// A pin the continuation does not carry is a mismatch: absence is not
    /// evidence of sameness.
    ///
    /// # Errors
    /// Returns [`ResumeRefusal::PinChanged`] or [`ResumeRefusal::LimitsChanged`].
    pub fn check_unchanged(
        &self,
        pins: &BTreeMap<String, String>,
        limits: TurnLimits,
    ) -> Result<(), ResumeRefusal> {
        for (name, recorded) in &self.pins {
            let current = pins.get(name).map_or("<absent>", String::as_str);
            if current != recorded {
                return Err(ResumeRefusal::PinChanged {
                    name: name.clone(),
                    recorded: recorded.clone(),
                    current: current.to_owned(),
                });
            }
        }
        for name in pins.keys() {
            if !self.pins.contains_key(name) {
                return Err(ResumeRefusal::PinChanged {
                    name: name.clone(),
                    recorded: "<absent>".to_owned(),
                    current: pins[name].clone(),
                });
            }
        }
        if limits == self.limits {
            Ok(())
        } else {
            Err(ResumeRefusal::LimitsChanged)
        }
    }

    /// Wall-clock seconds left of `limit_secs` once the spent time is removed.
    ///
    /// # Errors
    /// Returns [`ResumeRefusal::DeadlineSpent`] when nothing is left.
    pub fn remaining_secs(&self, limit_secs: u64) -> Result<u64, ResumeRefusal> {
        match limit_secs.checked_sub(self.elapsed_secs) {
            Some(left) if left > 0 => Ok(left),
            _ => Err(ResumeRefusal::DeadlineSpent {
                spent_secs: self.elapsed_secs,
                limit_secs,
            }),
        }
    }
}

/// Decide whether one interrupted attempt can be continued, and from where.
///
/// Continuation is limited to a complete tool-result checkpoint with no
/// unresolved effect or request and no dependence on shell state. Anything
/// else is refused with the reason, never repaired. `own_span_secs` is the
/// wall-clock span of this attempt's own records.
///
/// # Errors
/// Returns the [`ResumeRefusal`] that applies first.
pub fn plan_resume(
    history_id: &str,
    attempt: &AttemptReconstruction,
    own_span_secs: u64,
) -> Result<ResumePlan, ResumeRefusal> {
    if attempt.terminal.is_some() {
        return Err(ResumeRefusal::AlreadyTerminal);
    }
    if !attempt.unresolved_calls.is_empty() {
        return Err(ResumeRefusal::UnresolvedEffect {
            calls: attempt.unresolved_calls.clone(),
        });
    }
    if !attempt.unresolved_requests.is_empty() {
        return Err(ResumeRefusal::UnresolvedRequest {
            turns: attempt.unresolved_requests.clone(),
        });
    }
    if attempt
        .completed_calls
        .iter()
        .any(|c| c.tool == SHELL_TOOL_NAME)
    {
        return Err(ResumeRefusal::ShellState);
    }
    let checkpoint = attempt
        .last_checkpoint
        .as_ref()
        .ok_or(ResumeRefusal::NoCheckpoint)?;
    let log = checkpoint.log.clone().ok_or(ResumeRefusal::NoCheckpoint)?;
    if let Some(later) = attempt.assistants.iter().find(|a| a.turn > checkpoint.turn) {
        return Err(ResumeRefusal::IncompleteTurn { turn: later.turn });
    }
    let limits = attempt.limits.ok_or(ResumeRefusal::NoLimits)?;
    let inherited = attempt.resumed_from.as_ref();
    let requests_sent = inherited
        .map_or(0, |r| r.requests_sent)
        .saturating_add(attempt.requests_sent);
    Ok(ResumePlan {
        from_history_id: history_id.to_owned(),
        limits,
        pins: attempt.pins.clone(),
        next_turn: checkpoint.turn.saturating_add(1),
        tools_spent: attempt.tools_spent.max(checkpoint.tools_spent),
        requests_sent,
        elapsed_secs: inherited
            .map_or(0, |r| r.elapsed_secs)
            .saturating_add(own_span_secs),
        checkpoint_turn: checkpoint.turn,
        log,
    })
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
            TurnRecord::Resumed {
                from_history_id,
                checkpoint_turn,
                log,
                tools_spent,
                requests_sent,
                elapsed_secs,
            } => {
                if !*started || out.requests_sent > 0 || out.resumed_from.is_some() {
                    return Err(SequenceError::OutOfOrder(
                        "resumed must follow attempt_started and precede every request".to_owned(),
                    ));
                }
                out.tools_spent = out.tools_spent.max(*tools_spent);
                // The restored log is this attempt's baseline boundary until
                // it completes one of its own.
                out.last_checkpoint = Some(CheckpointRef {
                    turn: *checkpoint_turn,
                    log: Some(log.clone()),
                    tools_spent: *tools_spent,
                });
                out.resumed_from = Some(ResumedFrom {
                    history_id: from_history_id.clone(),
                    checkpoint_turn: *checkpoint_turn,
                    requests_sent: *requests_sent,
                    elapsed_secs: *elapsed_secs,
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

    // -- resume planning ---------------------------------------------------

    fn blob(kind: BlobKind, bytes: &[u8]) -> BlobRef {
        BlobRef {
            kind,
            digest: BlobDigest::of(bytes),
            len: bytes.len() as u64,
            schema_version: HARNESS_TURN_SCHEMA_VERSION,
        }
    }

    /// One complete turn: request, envelope, intent, receipt, checkpoint.
    fn complete_turn(turn: u32, tool: &str, spent: u32) -> Vec<TurnRecord> {
        vec![
            intent(turn, spent.saturating_sub(1)),
            TurnRecord::AssistantReceived {
                turn,
                calls: vec![CallEvidence {
                    call_id: format!("c{turn}"),
                    tool: tool.to_owned(),
                }],
                envelope: None,
                tools_spent: spent.saturating_sub(1),
            },
            TurnRecord::ToolIntent {
                turn,
                call_id: format!("c{turn}"),
                tool: tool.to_owned(),
                arguments_digest: BlobDigest::of(b"{}"),
                tools_spent: spent,
            },
            TurnRecord::ToolReceipt {
                turn,
                call_id: format!("c{turn}"),
                outcome: TurnToolOutcome::Succeeded,
                result: None,
                tools_spent: spent,
            },
            TurnRecord::Checkpoint {
                turn,
                log: Some(blob(BlobKind::LogCheckpoint, b"log")),
                tools_spent: spent,
            },
        ]
    }

    fn plan_of(records: &[TurnRecord]) -> Result<ResumePlan, ResumeRefusal> {
        let attempt = reconstruct(records).expect("ordered");
        plan_resume("h1", &attempt, 5)
    }

    #[test]
    fn a_complete_file_tool_checkpoint_resumes_with_spent_counters() {
        let mut records = vec![started()];
        records.extend(complete_turn(0, "write_file", 1));
        records.extend(complete_turn(1, "read_file", 2));
        let plan = plan_of(&records).expect("safe");
        assert_eq!(plan.next_turn, 2);
        assert_eq!(plan.tools_spent, 2);
        assert_eq!(plan.requests_sent, 2);
        assert_eq!(plan.checkpoint_turn, 1);
        assert_eq!(plan.elapsed_secs, 5);
        assert_eq!(plan.from_history_id, "h1");
    }

    #[test]
    fn an_effect_with_no_receipt_is_never_planned_for_replay() {
        let mut records = vec![started()];
        records.extend(complete_turn(0, "write_file", 1));
        records.extend([
            intent(1, 1),
            TurnRecord::AssistantReceived {
                turn: 1,
                calls: vec![CallEvidence {
                    call_id: "c1".to_owned(),
                    tool: SHELL_TOOL_NAME.to_owned(),
                }],
                envelope: None,
                tools_spent: 1,
            },
            TurnRecord::ToolIntent {
                turn: 1,
                call_id: "c1".to_owned(),
                tool: SHELL_TOOL_NAME.to_owned(),
                arguments_digest: BlobDigest::of(b"{}"),
                tools_spent: 2,
            },
        ]);
        let refusal = plan_of(&records).expect_err("unknown effect");
        assert!(
            matches!(refusal, ResumeRefusal::UnresolvedEffect { ref calls } if calls.len() == 1)
        );
        assert!(refusal.to_string().contains("unknown"));
    }

    #[test]
    fn a_request_with_no_outcome_needs_reconciliation() {
        let mut records = vec![started()];
        records.extend(complete_turn(0, "write_file", 1));
        records.push(intent(1, 1));
        assert_eq!(
            plan_of(&records),
            Err(ResumeRefusal::UnresolvedRequest { turns: vec![1] })
        );
    }

    #[test]
    fn a_completed_shell_call_blocks_resume_because_its_state_is_not_recorded() {
        let mut records = vec![started()];
        records.extend(complete_turn(0, SHELL_TOOL_NAME, 1));
        assert_eq!(plan_of(&records), Err(ResumeRefusal::ShellState));
    }

    #[test]
    fn no_checkpoint_a_terminal_response_and_a_dangling_turn_are_refused() {
        assert_eq!(
            plan_of(&[started(), intent(0, 0)])
                .map_err(|e| matches!(e, ResumeRefusal::UnresolvedRequest { .. })),
            Err(true)
        );
        let mut no_checkpoint = vec![started(), intent(0, 0)];
        no_checkpoint.push(TurnRecord::RequestFailed {
            turn: 0,
            reason: "boom".to_owned(),
        });
        assert_eq!(plan_of(&no_checkpoint), Err(ResumeRefusal::NoCheckpoint));

        let mut ended = vec![started()];
        ended.extend(complete_turn(0, "write_file", 1));
        ended.extend([
            intent(1, 1),
            TurnRecord::Terminal {
                turn: 1,
                disposition: TerminalKind::Normal,
                partial_text: None,
                tools_spent: 1,
            },
        ]);
        assert_eq!(plan_of(&ended), Err(ResumeRefusal::AlreadyTerminal));

        let mut dangling = vec![started()];
        dangling.extend(complete_turn(0, "write_file", 1));
        dangling.extend([
            intent(1, 1),
            TurnRecord::AssistantReceived {
                turn: 1,
                calls: Vec::new(),
                envelope: None,
                tools_spent: 1,
            },
        ]);
        assert_eq!(
            plan_of(&dangling),
            Err(ResumeRefusal::IncompleteTurn { turn: 1 })
        );
    }

    #[test]
    fn a_resumed_attempt_inherits_the_checkpoint_and_every_spent_budget() {
        let log = blob(BlobKind::LogCheckpoint, b"log");
        let records = vec![
            started(),
            TurnRecord::Resumed {
                from_history_id: "h0".to_owned(),
                checkpoint_turn: 3,
                log: log.clone(),
                tools_spent: 4,
                requests_sent: 5,
                elapsed_secs: 100,
            },
        ];
        let plan = plan_of(&records).expect("the restored boundary is the baseline");
        assert_eq!(plan.next_turn, 4);
        assert_eq!(plan.tools_spent, 4);
        assert_eq!(plan.requests_sent, 5);
        assert_eq!(plan.elapsed_secs, 105);
        assert_eq!(plan.log, log);
    }

    #[test]
    fn a_resumed_record_after_a_request_is_out_of_order() {
        let records = vec![
            started(),
            intent(0, 0),
            TurnRecord::Resumed {
                from_history_id: "h0".to_owned(),
                checkpoint_turn: 0,
                log: blob(BlobKind::LogCheckpoint, b"log"),
                tools_spent: 0,
                requests_sent: 1,
                elapsed_secs: 0,
            },
        ];
        assert!(matches!(
            reconstruct(&records),
            Err(SequenceError::OutOfOrder(_))
        ));
    }

    #[test]
    fn a_changed_pin_or_ceiling_invalidates_the_continuation() {
        let mut pins = BTreeMap::new();
        pins.insert("requested_model".to_owned(), "m1".to_owned());
        let mut records = vec![TurnRecord::AttemptStarted {
            limits: limits(),
            pins: pins.clone(),
        }];
        records.extend(complete_turn(0, "write_file", 1));
        let plan = plan_of(&records).expect("safe");
        assert_eq!(plan.check_unchanged(&pins, limits()), Ok(()));

        let mut other = pins.clone();
        other.insert("requested_model".to_owned(), "m2".to_owned());
        assert!(matches!(
            plan.check_unchanged(&other, limits()),
            Err(ResumeRefusal::PinChanged { ref name, .. }) if name == "requested_model"
        ));
        assert!(matches!(
            plan.check_unchanged(&BTreeMap::new(), limits()),
            Err(ResumeRefusal::PinChanged { .. })
        ));
        let mut extra = pins.clone();
        extra.insert("worktree".to_owned(), "w".to_owned());
        assert!(matches!(
            plan.check_unchanged(&extra, limits()),
            Err(ResumeRefusal::PinChanged { .. })
        ));
        let mut raised = limits();
        raised.max_tool_calls += 1;
        assert_eq!(
            plan.check_unchanged(&pins, raised),
            Err(ResumeRefusal::LimitsChanged)
        );
    }

    #[test]
    fn the_deadline_only_ever_shrinks() {
        let mut records = vec![started()];
        records.extend(complete_turn(0, "write_file", 1));
        let plan = plan_of(&records).expect("safe");
        assert_eq!(plan.remaining_secs(60), Ok(55));
        assert_eq!(
            plan.remaining_secs(5),
            Err(ResumeRefusal::DeadlineSpent {
                spent_secs: 5,
                limit_secs: 5
            })
        );
    }

    #[test]
    fn reasons_are_bounded() {
        assert_eq!(
            bounded_reason(&"é".repeat(2000)).chars().count(),
            MAX_REASON_CHARS
        );
    }
}
