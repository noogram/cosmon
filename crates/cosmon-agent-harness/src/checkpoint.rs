// SPDX-License-Identifier: AGPL-3.0-only

//! The loop's side of durable turn evidence.
//!
//! [`TurnJournal`] turns what the spine does into the typed records of
//! [`cosmon_core::harness_turn`] and hands them to a
//! [`TurnEvidenceStore`]. It owns no storage and no ordering of its own: the
//! store is the port, and the order of calls is the order of the ledger.
//!
//! Every method returns the store's error unchanged. The spine treats an error
//! from a method that precedes an effect as a reason not to perform it, so a
//! record that is not durable never licenses the side effect it announces.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use cosmon_core::harness_turn::{
    bounded_reason, BlobDigest, BlobKind, BlobRef, CallEvidence, EvidenceError, InputEvidence,
    ResumePlan, TerminalKind, TurnEvidenceStore, TurnLimits, TurnRecord, TurnToolOutcome,
};

use crate::budget::LoopBudget;
use crate::compaction::CompactionReport;
use crate::spine::TerminalDisposition;
use crate::tool::{ToolCall, ToolOutcome};

/// Records one worker attempt's turns and tool effects.
pub struct TurnJournal {
    store: Arc<dyn TurnEvidenceStore>,
    /// Turn of the most recent request intent, so input selection made inside
    /// the provider call is filed under the request it rides on.
    current_turn: AtomicU32,
    /// Pins the caller knows and the loop does not, such as the requested model.
    pins: BTreeMap<String, String>,
}

impl fmt::Debug for TurnJournal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnJournal")
            .field("current_turn", &self.current_turn.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl TurnJournal {
    /// Wrap a store.
    #[must_use]
    pub fn new(store: Arc<dyn TurnEvidenceStore>) -> Self {
        Self {
            store,
            current_turn: AtomicU32::new(0),
            pins: BTreeMap::new(),
        }
    }

    /// Add a named pin to the `attempt_started` record, such as the requested
    /// model or the adapter. A pin the loop sets itself takes precedence.
    #[must_use]
    pub fn with_pin(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.pins.insert(name.into(), value.into());
        self
    }

    /// The pins the `attempt_started` record will carry: the caller's, with
    /// the loop's own taking precedence. A continuation compares these against
    /// what the attempt recorded.
    #[must_use]
    pub fn effective_pins(&self, loop_pins: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut merged = self.pins.clone();
        merged.extend(loop_pins.iter().map(|(k, v)| (k.clone(), v.clone())));
        merged
    }

    fn blob(&self, kind: BlobKind, bytes: Option<&[u8]>) -> Result<Option<BlobRef>, EvidenceError> {
        bytes.map(|b| self.store.put_blob(kind, b)).transpose()
    }

    /// Record the start of the attempt with its ceilings and pins.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn started(
        &self,
        budget: LoopBudget,
        pins: BTreeMap<String, String>,
    ) -> Result<(), EvidenceError> {
        let mut merged = self.pins.clone();
        merged.extend(pins);
        let pins = merged;
        self.store.append(TurnRecord::AttemptStarted {
            limits: TurnLimits {
                max_turns: budget.turns.max_turns,
                max_tool_calls: budget.tools.max_tool_calls,
                max_input_tokens: budget.context.max_input_tokens,
            },
            pins,
        })
    }

    /// Record that this attempt continues `plan`'s attempt, directly after
    /// `attempt_started` and before the first request.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn resumed(&self, plan: &ResumePlan) -> Result<(), EvidenceError> {
        self.store.append(TurnRecord::Resumed {
            from_history_id: plan.from_history_id.clone(),
            checkpoint_turn: plan.checkpoint_turn,
            log: plan.log.clone(),
            tools_spent: plan.tools_spent,
            requests_sent: plan.requests_sent,
            elapsed_secs: plan.elapsed_secs,
        })
    }

    /// Record that a request is about to be sent.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn request_intent(
        &self,
        turn: u32,
        estimated_input_tokens: u32,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        self.current_turn.store(turn, Ordering::Relaxed);
        self.store.append(TurnRecord::RequestIntent {
            turn,
            estimated_input_tokens,
            tools_spent,
        })
    }

    /// Record the input blocks selected for the current request.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn inputs_selected(&self, inputs: Vec<InputEvidence>) -> Result<(), EvidenceError> {
        self.store.append(TurnRecord::InputsSelected {
            turn: self.current_turn.load(Ordering::Relaxed),
            inputs,
        })
    }

    /// Record that the current request produced no usable response.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn request_failed(&self, reason: &str) -> Result<(), EvidenceError> {
        self.store.append(TurnRecord::RequestFailed {
            turn: self.current_turn.load(Ordering::Relaxed),
            reason: bounded_reason(reason),
        })
    }

    /// Record a compaction that preceded the current request.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable.
    pub fn compacted(&self, turn: u32, report: &CompactionReport) -> Result<(), EvidenceError> {
        let clamp = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        self.store.append(TurnRecord::Compacted {
            turn,
            tokens_before: report.tokens_before,
            tokens_after: report.tokens_after,
            messages_removed: clamp(report.messages_removed),
        })
    }

    /// Record a valid assistant envelope, before any of its tools run.
    ///
    /// # Errors
    /// Returns the store's error if the envelope or record is not durable.
    pub fn assistant_received(
        &self,
        turn: u32,
        calls: &[ToolCall],
        envelope: Option<&[u8]>,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        let envelope = self.blob(BlobKind::AssistantEnvelope, envelope)?;
        self.store.append(TurnRecord::AssistantReceived {
            turn,
            calls: calls
                .iter()
                .map(|c| CallEvidence {
                    call_id: c.id.clone(),
                    tool: c.name.clone(),
                })
                .collect(),
            envelope,
            tools_spent,
        })
    }

    /// Record a tool call, before its effect.
    ///
    /// # Errors
    /// Returns the store's error if the record is not durable. The caller must
    /// not run the tool then.
    pub fn tool_intent(
        &self,
        turn: u32,
        call: &ToolCall,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        self.store.append(TurnRecord::ToolIntent {
            turn,
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments_digest: BlobDigest::of(call.arguments_json.as_bytes()),
            tools_spent,
        })
    }

    /// Record a tool result, before the loop advances.
    ///
    /// # Errors
    /// Returns the store's error if the result or record is not durable.
    pub fn tool_receipt(
        &self,
        turn: u32,
        call: &ToolCall,
        outcome: ToolOutcome,
        result: Option<&str>,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        let result = self.blob(BlobKind::ToolResult, result.map(str::as_bytes))?;
        self.store.append(TurnRecord::ToolReceipt {
            turn,
            call_id: call.id.clone(),
            outcome: match outcome {
                ToolOutcome::Succeeded => TurnToolOutcome::Succeeded,
                ToolOutcome::Failed => TurnToolOutcome::Failed,
                ToolOutcome::Uncertain => TurnToolOutcome::Uncertain,
            },
            result,
            tools_spent,
        })
    }

    /// Record a complete tool-result boundary with the native log.
    ///
    /// # Errors
    /// Returns the store's error if the checkpoint or record is not durable.
    pub fn checkpoint(
        &self,
        turn: u32,
        log: Option<&[u8]>,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        let log = self.blob(BlobKind::LogCheckpoint, log)?;
        self.store.append(TurnRecord::Checkpoint {
            turn,
            log,
            tools_spent,
        })
    }

    /// Record the terminal response and the text received before it.
    ///
    /// # Errors
    /// Returns the store's error if the text or record is not durable.
    pub fn terminal(
        &self,
        turn: u32,
        disposition: &TerminalDisposition,
        text: &str,
        tools_spent: u32,
    ) -> Result<(), EvidenceError> {
        let partial_text = if text.is_empty() {
            None
        } else {
            self.blob(BlobKind::PartialText, Some(text.as_bytes()))?
        };
        self.store.append(TurnRecord::Terminal {
            turn,
            disposition: match disposition {
                TerminalDisposition::Normal => TerminalKind::Normal,
                TerminalDisposition::OutputLimit => TerminalKind::OutputLimit,
                TerminalDisposition::Refused => TerminalKind::Refused,
                TerminalDisposition::Incomplete => TerminalKind::Incomplete,
                TerminalDisposition::Unknown(reason) => TerminalKind::Unknown {
                    reason: bounded_reason(reason),
                },
            },
            partial_text,
            tools_spent,
        })
    }
}
