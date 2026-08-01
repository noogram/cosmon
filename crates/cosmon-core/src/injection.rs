// SPDX-License-Identifier: AGPL-3.0-only

//! Provenance vocabulary for keystroke injection into a worker session
//! (COSMON #26 residual).
//!
//! # The question this answers
//!
//! An operator found the literal text `cs done` sitting **unsubmitted** in a
//! worker's composer (issue #26). No production path was found that sends that
//! string as TUI input, so nothing here is a fix: it is the instrument that
//! makes the next occurrence self-explaining. Today, text that appears in a
//! composer is anonymous — the pane shows *what* arrived and never *who* sent
//! it, and the six senders (tackle briefing, patrol nudge, thaw, resume,
//! propulsion, briefing backstop) are indistinguishable after the fact.
//!
//! Every injection into a tmux worker funnels through exactly one function,
//! `TmuxBackend::send_input_observed`. Attaching an [`InjectionProvenance`] to
//! that call and recording it as an
//! [`EventV2::InputInjected`](crate::event_v2::EventV2::InputInjected) turns
//! the pane's anonymous text into a named, timestamped ledger line.
//!
//! # What is recorded, and what is deliberately not
//!
//! The event carries the [`InjectionOrigin`], a free-form [`purpose`] label,
//! the target session, the input **length**, and a truncated BLAKE3
//! [`injection_digest`] — never the input itself. A briefing is confidential
//! by construction (it can quote private material), and `events.jsonl` is
//! tracked in git. The digest is enough to answer the forensic question that
//! matters: *is the text in this composer the text cosmon sent?* — compare the
//! digest of the suspect string against the ledger. It is not enough to
//! reconstruct the text, which is the point.
//!
//! # The bare submit is covered too
//!
//! Half of cosmon's injections are an empty input: a naked Enter that flushes a
//! composer someone else filled. A stray one aimed at the wrong session is the
//! *least* traceable event of all — it leaves no text behind at all. So the
//! empty input is not a skipped case here; it is flagged explicitly by
//! [`InjectionProvenance::bare_submit`].
//!
//! [`purpose`]: InjectionProvenance::purpose

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::id::MoleculeId;

/// Number of hex characters kept from the BLAKE3 digest of an injected input.
///
/// 16 hex chars = 64 bits. Enough that two distinct injections colliding is not
/// a practical concern for a forensic comparison, short enough that an operator
/// can eyeball it in a `jq` line next to the other fields.
const DIGEST_HEX_LEN: usize = 16;

/// Fingerprint an injected input for the ledger — **never** the input itself.
///
/// Truncated BLAKE3 hex (16 chars, see `DIGEST_HEX_LEN`). The empty input has a
/// perfectly well-defined digest and is fingerprinted like any other, so a bare
/// submit is a full row in the log rather than a hole in it.
///
/// # Examples
///
/// ```
/// use cosmon_core::injection::injection_digest;
///
/// // Same bytes in, same fingerprint out — that is the whole forensic use:
/// // digest the string found in a composer, grep the ledger for it.
/// assert_eq!(injection_digest("cs done"), injection_digest("cs done"));
/// assert_ne!(injection_digest("cs done"), injection_digest("cs evolve"));
/// assert_eq!(injection_digest("cs done").len(), 16);
/// ```
#[must_use]
pub fn injection_digest(input: &str) -> String {
    let mut hex = cosmon_hash::Hash::of_bytes(input.as_bytes()).to_hex();
    hex.truncate(DIGEST_HEX_LEN);
    hex
}

/// Which caller drove an injection into a worker session.
///
/// The load-bearing field of the provenance event: it is the answer to "who
/// wrote this?" for text found in a composer. Every in-tree caller of the send
/// seam names itself with one of these; [`Self::Unattributed`] is the honest
/// value for a caller that went through the plain
/// [`TransportBackend::send_input`](crate::transport::TransportBackend::send_input)
/// without declaring itself, and its presence in the log is itself a finding —
/// it names an uninstrumented path rather than inventing an origin for it.
///
/// `#[non_exhaustive]` because the set of senders grows with the CLI. Wire
/// format is the `snake_case` variant name.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionOrigin {
    /// `cs tackle` delivering a freshly-spawned worker its briefing.
    TackleBriefing,
    /// `cs patrol` nudging a worker it judged silent but alive.
    PatrolNudge,
    /// `cs patrol --heal` re-briefing a worker it re-attached to.
    PatrolHeal,
    /// Propulsion: the periodic "keep going" signal to a running worker.
    Propulsion,
    /// `cs thaw` handing a resumed molecule its continuation prompt.
    Thaw,
    /// `cs resume` restoring a worker after a session restart.
    Resume,
    /// The briefing backstop — a cross-process Enter for a briefing whose
    /// dispatcher exited before the composer cleared (COSMON #26-B).
    BriefingBackstop,
    /// `cs whisper` — operator-authored text sent to a live worker.
    Whisper,
    /// A readiness probe pressing Enter to see whether a pane answers.
    ReadinessProbe,
    /// `cs patrol`'s opt-in dialogue auto-confirm: a bare Enter that accepts a
    /// TUI permission prompt's highlighted default.
    DialogueAutoConfirm,
    /// The graceful-exit path sending the adapter's quit command.
    GracefulExit,
    /// No caller declared itself: the injection arrived through the plain
    /// `send_input` port method. The default, and never a good sign in a log.
    #[default]
    Unattributed,
}

impl InjectionOrigin {
    /// The wire spelling, for log lines and `jq` filters.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TackleBriefing => "tackle_briefing",
            Self::PatrolNudge => "patrol_nudge",
            Self::PatrolHeal => "patrol_heal",
            Self::Propulsion => "propulsion",
            Self::Thaw => "thaw",
            Self::Resume => "resume",
            Self::BriefingBackstop => "briefing_backstop",
            Self::Whisper => "whisper",
            Self::ReadinessProbe => "readiness_probe",
            Self::DialogueAutoConfirm => "dialogue_auto_confirm",
            Self::GracefulExit => "graceful_exit",
            Self::Unattributed => "unattributed",
        }
    }
}

impl std::fmt::Display for InjectionOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the provenance event is to be written, when the caller knows.
///
/// The send seam lives in `cosmon-transport` and holds a tmux socket, not a
/// galaxy: it cannot discover a molecule's event log on its own. The caller —
/// which is always holding the molecule it is nudging — supplies the pair.
/// Absent it, the seam still traces the injection, it simply has no ledger to
/// append to; see [`InjectionProvenance::ledger`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionLedger {
    /// The molecule whose worker is being injected into.
    pub mol_id: MoleculeId,
    /// Directory holding that molecule's `events.jsonl`.
    pub state_dir: PathBuf,
}

impl InjectionLedger {
    /// Bind a molecule to the directory holding its event log.
    #[must_use]
    pub fn new(mol_id: MoleculeId, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            mol_id,
            state_dir: state_dir.into(),
        }
    }

    /// The directory the event is appended under.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
}

/// Everything the send seam records about one injection, minus the input.
///
/// Constructed by the caller, consumed by
/// `TmuxBackend::send_input_observed`. The input's length and digest are *not*
/// fields here — the seam derives them from the bytes it is about to send, so
/// they cannot disagree with what was actually injected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionProvenance {
    /// Which caller drove this injection.
    pub origin: InjectionOrigin,
    /// Short free-form label for *why*, at finer grain than `origin`
    /// (`"briefing"`, `"submit-retry"`, `"propel-nudge"`). Kept as text
    /// because the reason a given caller sends bytes changes faster than the
    /// set of callers does, and a stale enum variant is worse than a string.
    pub purpose: String,
    /// Where to append the event, when the caller knows. `None` degrades the
    /// seam to a `tracing` line: an injection is never blocked because its
    /// provenance has nowhere to land.
    pub ledger: Option<InjectionLedger>,
}

impl InjectionProvenance {
    /// Declare an injection's origin and purpose, with no ledger yet.
    #[must_use]
    pub fn new(origin: InjectionOrigin, purpose: impl Into<String>) -> Self {
        Self {
            origin,
            purpose: purpose.into(),
            ledger: None,
        }
    }

    /// Attach the molecule event log this injection should be recorded in.
    #[must_use]
    pub fn with_ledger(mut self, ledger: InjectionLedger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Attach a ledger only if the caller managed to resolve one.
    #[must_use]
    pub fn with_ledger_opt(mut self, ledger: Option<InjectionLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// The provenance of an injection whose caller did not declare itself.
    ///
    /// What `send_input` (the plain port method) stamps. Deliberately not
    /// nameless: an unattributed row in the ledger names the uninstrumented
    /// path, which is the next thing to fix rather than something to hide.
    #[must_use]
    pub fn unattributed() -> Self {
        Self::new(InjectionOrigin::Unattributed, "send_input")
    }

    /// Whether an input of this length is a bare submit — a naked Enter with
    /// no text of its own.
    ///
    /// The case the pane cannot testify about after the fact, so the ledger
    /// must.
    #[must_use]
    pub fn bare_submit(input: &str) -> bool {
        input.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_truncated_and_never_echoes_the_input() {
        let secret = "an operator's private briefing line";
        let digest = injection_digest(secret);
        assert_eq!(digest.len(), DIGEST_HEX_LEN);
        assert!(!digest.contains("operator"));
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_empty_input_still_has_a_digest() {
        // The bare submit is a row in the ledger, not a hole in it.
        assert_eq!(injection_digest("").len(), DIGEST_HEX_LEN);
        assert_ne!(injection_digest(""), injection_digest(" "));
        assert!(InjectionProvenance::bare_submit(""));
        assert!(!InjectionProvenance::bare_submit(" "));
    }

    #[test]
    fn an_undeclared_caller_is_named_unattributed() {
        let p = InjectionProvenance::unattributed();
        assert_eq!(p.origin, InjectionOrigin::Unattributed);
        assert_eq!(p.origin.as_str(), "unattributed");
        assert!(p.ledger.is_none());
    }

    #[test]
    fn origin_wire_spelling_matches_serde() {
        for origin in [
            InjectionOrigin::TackleBriefing,
            InjectionOrigin::PatrolNudge,
            InjectionOrigin::PatrolHeal,
            InjectionOrigin::Propulsion,
            InjectionOrigin::Thaw,
            InjectionOrigin::Resume,
            InjectionOrigin::BriefingBackstop,
            InjectionOrigin::Whisper,
            InjectionOrigin::ReadinessProbe,
            InjectionOrigin::DialogueAutoConfirm,
            InjectionOrigin::GracefulExit,
            InjectionOrigin::Unattributed,
        ] {
            let json = serde_json::to_string(&origin).unwrap();
            assert_eq!(json, format!("\"{}\"", origin.as_str()));
            let back: InjectionOrigin = serde_json::from_str(&json).unwrap();
            assert_eq!(back, origin);
        }
    }
}
