// SPDX-License-Identifier: AGPL-3.0-only

//! Stable callsite for the injection-provenance event (COSMON #26 residual).
//!
//! One free function, [`emit_input_injected`], called from the single seam
//! every tmux keystroke injection passes through
//! (`cosmon_transport::tmux::TmuxBackend::send_input_observed`). It answers the
//! question a composer cannot: *who wrote this text?*
//!
//! # No new writer
//!
//! This module deliberately owns no persistence of its own. It reuses
//! `super::worker_spawn::write_event` (private, hence not linked), and
//! therefore inherits the whole discipline already built there: the canonical
//! `events.jsonl` append, the
//! `events.error.jsonl` sidecar when that append fails, the process-wide
//! error counter, and the loud-but-once stderr line. Adding a second writer
//! would give injection provenance a different failure mode from every other
//! event family — which is exactly the drift that makes a ledger untrustworthy.
//!
//! Best-effort, like every emission helper here: an injection is never blocked
//! because its provenance could not be written.

use std::path::Path;

use cosmon_core::event_v2::EventV2;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::injection::{injection_digest, InjectionOrigin};

use super::worker_spawn::write_event;

/// Emit an [`EventV2::InputInjected`] — the provenance of one keystroke
/// injection into a worker session.
///
/// Called by the send seam **before** the bytes go out, so an injection that
/// then fails to land (session gone, tmux refused) is still attributed. The
/// alternative — emitting on success — would leave exactly the misfires
/// unexplained, and a misfire aimed at the wrong session is the case this
/// instrument exists for.
///
/// # Confidentiality
///
/// `input` is consumed **only** to derive its length and its truncated BLAKE3
/// fingerprint via [`injection_digest`]. The content is never written to the
/// ledger, which is tracked in git and may otherwise republish a private
/// briefing. Deriving both here rather than taking them as parameters is what
/// makes that guarantee structural: a caller cannot hand this function a
/// "digest" that is really the plaintext.
///
/// # Examples
///
/// ```
/// use cosmon_core::id::{MoleculeId, WorkerId};
/// use cosmon_core::injection::InjectionOrigin;
/// use cosmon_state::events::input_injection::emit_input_injected;
///
/// let dir = tempfile::tempdir().unwrap();
/// emit_input_injected(
///     dir.path(),
///     Some(&MoleculeId::new("task-20260731-f0ab").unwrap()),
///     &WorkerId::new("polecat-1234").unwrap(),
///     "cs-polecat-1234",
///     InjectionOrigin::PatrolNudge,
///     "propel-nudge",
///     "keep going",
/// );
///
/// let log = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
/// assert!(log.contains("input_injected"));
/// // The text itself never reaches the ledger — only its fingerprint.
/// assert!(!log.contains("keep going"));
/// ```
pub fn emit_input_injected(
    state_dir: &Path,
    mol_id: Option<&MoleculeId>,
    worker_id: &WorkerId,
    session: &str,
    origin: InjectionOrigin,
    purpose: &str,
    input: &str,
) {
    let event = EventV2::InputInjected {
        mol_id: mol_id.cloned(),
        worker_id: worker_id.clone(),
        session: session.to_owned(),
        origin,
        purpose: purpose.to_owned(),
        input_len: input.len(),
        input_digest: injection_digest(input),
        // The bare submit — an empty input whose only effect is the submit
        // keystroke. Flagged rather than inferred from `input_len == 0` by the
        // reader, because that inference is exactly what a later refactor
        // silently gets wrong.
        bare_submit: input.is_empty(),
    };
    write_event(state_dir, event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::Envelope;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260731-f0ab").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-1234").unwrap()
    }

    fn read_events(state_dir: &Path) -> Vec<Envelope> {
        let raw = std::fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Envelope::from_line(l).expect("parse envelope"))
            .collect()
    }

    #[test]
    fn the_ledger_names_the_writer_and_never_the_text() {
        let dir = tempdir().unwrap();
        let secret = "the operator's confidential briefing line";
        emit_input_injected(
            dir.path(),
            Some(&mol()),
            &wkr(),
            "cs-polecat-1234",
            InjectionOrigin::TackleBriefing,
            "briefing",
            secret,
        );

        let events = read_events(dir.path());
        assert_eq!(events.len(), 1, "exactly one event per injection");
        let EventV2::InputInjected {
            mol_id,
            worker_id,
            session,
            origin,
            purpose,
            input_len,
            input_digest,
            bare_submit,
        } = &events[0].event
        else {
            panic!("expected InputInjected, got {:?}", events[0].event);
        };
        assert_eq!(mol_id.as_ref(), Some(&mol()));
        assert_eq!(worker_id, &wkr());
        assert_eq!(session, "cs-polecat-1234");
        assert_eq!(*origin, InjectionOrigin::TackleBriefing);
        assert_eq!(purpose, "briefing");
        assert_eq!(*input_len, secret.len());
        assert_eq!(input_digest, &injection_digest(secret));
        assert!(!*bare_submit);

        // The confidentiality claim, asserted against the bytes on disk.
        let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        assert!(!raw.contains("confidential briefing line"));
    }

    #[test]
    fn a_bare_submit_is_a_row_not_a_hole() {
        let dir = tempdir().unwrap();
        emit_input_injected(
            dir.path(),
            None,
            &wkr(),
            "cs-polecat-1234",
            InjectionOrigin::BriefingBackstop,
            "bare-submit",
            "",
        );

        let events = read_events(dir.path());
        assert_eq!(events.len(), 1);
        let EventV2::InputInjected {
            mol_id,
            input_len,
            bare_submit,
            origin,
            ..
        } = &events[0].event
        else {
            panic!("expected InputInjected");
        };
        assert!(mol_id.is_none(), "no molecule context is representable");
        assert_eq!(*input_len, 0);
        assert!(*bare_submit, "a naked Enter must be flagged as such");
        assert_eq!(*origin, InjectionOrigin::BriefingBackstop);
    }
}
