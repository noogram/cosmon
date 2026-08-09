// SPDX-License-Identifier: AGPL-3.0-only

//! `cs patrol --propel-api-stall` — re-engage a worker the **provider** says is
//! stalled, and no other worker.
//!
//! # The incident
//!
//! On 2026-08-09 two workers (`task-20260808-3e5c`, `task-20260808-3033`) sat
//! frozen on `API Error: Response stalled mid-stream.` The molecules were alive,
//! the tmux panes were up, and nothing in cosmon moved until the operator
//! attached each session by hand and typed `continue`. One word, twice,
//! manually — for a fault that leaves a machine-readable trace.
//!
//! # Why not just re-enable `cosmon-fleet-propel`
//!
//! Because its trigger was wrong, and the operator was right to disable it on
//! 2026-07-23: *a worker that is thinking is not a worker that is stuck*. That
//! patrol keyed on **apparent idleness**, an inference, and one false positive
//! turned into an orphan livelock that burned $151 (task-20260720-8b63).
//! Switching the same trigger back on would buy back the same bug.
//!
//! This sweep keys on a **fact the provider wrote down** instead: the last
//! assistant record of the session journal carries
//! `"isApiErrorMessage": true`. When a request dies in transport, the CLI
//! appends that record and the session sits at the prompt with nothing running.
//! There is no thinking to interrupt — the turn is over and no turn replaced it.
//!
//! # The trap this module is built around
//!
//! The other way to detect the freeze would be to look at the pane, where the
//! sentence is plainly visible. That is exactly what caused the be1e SEV-1: a
//! bash guard grepped tmux panes for a phrase and killed healthy workers,
//! because of three lines carrying the phrase, two were **users quoting it**.
//! This module reads no pane text at all. It reads a typed boolean on a typed
//! record ([`cosmon_session_probe::last_assistant_api_error`]) — a `user`
//! record containing the identical sentence normalises to a user message, which
//! has no such field. Use and mention are separated structurally, not by
//! cleverness in a regex.
//!
//! # What is preserved from `--propel`
//!
//! Everything. The typed flag is a *warrant to consider speaking*, never an
//! override:
//!
//! - the single admission judge, via
//!   [`cosmon_core::propel::decide_api_stall_nudge`] — operator gate, molecule
//!   status, orphan gate, pane clock, attempt ceiling, exponential backoff;
//! - the `propel-exhausted` tag when four spaced nudges changed nothing;
//! - the `propel-orphaned` tag + `cs notify` page for a brief-less worker;
//! - the shared propulsion ledger (`propel_count` / `last_propelled_at`), so
//!   this channel and `--propel` cannot double-nudge one stall;
//! - the ADR-137 §5 no-interference guard ([`heal_gate`]) — a worker a human is
//!   piloting, a `health:hold` molecule, and the `~/.cosmon/health.off`
//!   kill-switch all stop the sweep dead.
//!
//! The result is strictly narrower than `--propel`: every worker it would
//! propel, `--propel` would too, and almost every worker `--propel` would
//! propel, this one leaves alone.

use chrono::Utc;
use cosmon_core::event_v2::PerturbationChannel;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::patrol::{heal_gate, GuardConfig, HealBlockReason, HealGate, HealthRemedy};
use cosmon_core::propel::{
    decide_api_stall_nudge, EscalateReason, NudgeChannel, NudgeDecision, NudgeSkip,
};
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::PresenceStore;
use cosmon_session_probe::{
    last_assistant_api_error, ApiStall, DiscoveryFilter, ProbeRegistry, ProviderSessionRef,
};
use cosmon_state::events::worker_spawn::emit_adapter_pane_signature_checked;
use cosmon_state::{Fleet, MoleculeData, StateStore};
use cosmon_transport::claude::ADAPTER_NAME as CLAUDE_ADAPTER;
use cosmon_transport::registry::{default_registry, pane_current_command, pane_idle_seconds};
use cosmon_transport::TmuxBackend;
use std::path::Path;

use super::patrol::{
    build_propulsion_view, escalate_orphan, find_stale_running_molecules, mark_propel_exhausted,
    record_propel, PropelSweep,
};

/// The sentence sent to a worker whose provider journal says its last turn died
/// in transport.
///
/// Deliberately different from `PROPULSION_NUDGE`: that one tells a worker it
/// *appears* idle, which would be a false statement here and an invitation to
/// re-plan. This one names the fault, so a worker that reads it knows the gap
/// in its transcript is a lost response and not something it did.
pub(crate) const API_STALL_NUDGE: &str =
    "⚛ PROPULSION (transport) — your last turn ended in a provider API error, \
     not in a decision: the response was lost mid-stream. Nothing you did is wrong. \
     Resume your current step where it stopped and continue execution.";

/// What one `--propel-api-stall` sweep looked at and what it decided.
///
/// The `considered` / `unflagged` pair is the honesty clause: a sweep that
/// propels nothing must be able to say *how many workers it read the journal of
/// and found healthy*, otherwise "0 propelled" is indistinguishable from "the
/// sweep never ran" — which is precisely how a narrow trigger silently rots.
#[derive(Debug, Default)]
pub(crate) struct ApiStallSweep {
    /// Running molecules with a live worker whose journal was consulted.
    pub(crate) considered: usize,
    /// Of those, how many carried no typed API error on their last turn — the
    /// overwhelming majority, and the whole point.
    pub(crate) unflagged: usize,
    /// Candidates whose journal could not be attributed to exactly one session
    /// (none discovered, or several equally recent). Unknown is never a stall:
    /// `(worker, molecule, sessions_seen)`.
    pub(crate) unattributed: Vec<(WorkerId, MoleculeId, usize)>,
    /// Flagged candidates the §5 no-interference guard held back — a human is
    /// piloting, the molecule is on hold, or the kill-switch is set:
    /// `(worker, molecule, reason)`.
    pub(crate) guarded: Vec<(WorkerId, MoleculeId, HealBlockReason)>,
    /// The admission-control outcome for every flagged, unguarded candidate.
    pub(crate) sweep: PropelSweep,
}

/// Resolve the **one** provider session a worker is writing, or report why not.
///
/// Attribution is by working directory, read from *inside* each journal (never
/// decoded from a directory name), because that is the only link between a
/// cosmon worker and a provider session that neither side can fake: the worker's
/// `repo` is what `cs tackle` created, and the session's `cwd` is what the
/// provider recorded on every line.
///
/// A worktree can hold more than one journal — a worker that was resumed leaves
/// the old one behind. The live session is the one still being written, so the
/// newest `last_observed_at` wins, and a **tie or a missing timestamp is a
/// refusal**: two candidate journals mean we do not know which turn belongs to
/// the worker in front of us, and a nudge decided on the wrong transcript is
/// exactly the class of error this whole mechanism exists to avoid.
fn attribute_session(reg: &ProbeRegistry, cwd: &Path) -> Result<ProviderSessionRef, usize> {
    let filter = DiscoveryFilter {
        repo: None,
        cwd: Some(cwd.to_path_buf()),
    };
    let mut found = reg.discover(&filter).unwrap_or_default();
    if found.is_empty() {
        return Err(0);
    }
    found.sort_by_key(|s| s.last_observed_at);
    let seen = found.len();
    let newest = found.pop().ok_or(seen)?;
    let Some(newest_at) = newest.last_observed_at else {
        // No mtime at all: nothing distinguishes this journal from its
        // neighbours, and with a single candidate there is still no evidence it
        // is live. Refuse rather than assume.
        return Err(seen);
    };
    if found
        .last()
        .is_some_and(|runner_up| runner_up.last_observed_at == Some(newest_at))
    {
        return Err(seen);
    }
    Ok(newest)
}

/// Read the worker's journal and answer the one question: did the provider type
/// the last assistant turn as a transport failure?
///
/// Returns `None` when the journal cannot be attributed to exactly one session,
/// carrying the number of candidates seen for the report. An I/O failure while
/// reading an attributed journal degrades to [`ApiStall::default()`] — unknown
/// is not a stall.
fn journal_verdict(reg: &ProbeRegistry, cwd: &Path) -> Result<ApiStall, usize> {
    let session = attribute_session(reg, cwd)?;
    let Some(probe) = reg.probe_for(&session.provider) else {
        return Err(1);
    };
    Ok(last_assistant_api_error(probe, &session).unwrap_or_default())
}

/// Everything one sweep needs, gathered by the caller.
///
/// A struct rather than eight positional parameters: the caller is `cs patrol`,
/// which already holds every one of these, and a parameter list that long is
/// where a `store` and a `state_dir` get silently swapped.
pub(crate) struct SweepInputs<'a> {
    /// Where molecule state is read and written.
    pub(crate) store: &'a dyn StateStore,
    /// `.cosmon/state` — presence rows for the §5.1 pilot clause live here.
    pub(crate) state_dir: &'a Path,
    /// Resolves workers' repo-relative working directories.
    pub(crate) project_root: &'a Path,
    /// The molecules of this pass.
    pub(crate) molecules: &'a [MoleculeData],
    /// The fleet, for worker desired-state and working directory.
    pub(crate) fleet: &'a Fleet,
    /// Transport. `None` (state-only mode) makes the sweep a no-op — this
    /// channel's only action is a keystroke.
    pub(crate) backend: Option<&'a TmuxBackend>,
    /// The session-probe registry — `cs sessions`' own, injected so a test can
    /// point it at a journal tree it owns.
    pub(crate) reg: &'a ProbeRegistry,
    /// Terminal-silence bar and backoff base, in seconds. Never a candidacy
    /// test here: candidacy is the provider's typed flag.
    pub(crate) stale_after: u64,
}

/// One sweep: for every live worker on a running molecule, consult the provider
/// journal and propel **iff** it says the last turn died in transport.
pub(crate) fn propel_api_stalled_molecules(inputs: &SweepInputs<'_>) -> ApiStallSweep {
    let mut out = ApiStallSweep::default();
    let Some(be) = inputs.backend else {
        return out;
    };
    let store = inputs.store;
    let now = Utc::now();
    // Every running molecule with a live-desired worker: staleness is *not* a
    // precondition here, because the journal flag is the evidence and a
    // control-plane clock would only re-introduce the inference.
    let candidates = find_stale_running_molecules(inputs.molecules, inputs.fleet, 0, now);
    let stale_window =
        chrono::Duration::seconds(i64::try_from(inputs.stale_after).unwrap_or(i64::MAX));
    let presences = PresenceStore::new(inputs.state_dir)
        .scan()
        .unwrap_or_default();
    let guard_cfg = GuardConfig::default();
    let kill_switched = super::patrol_heal::global_kill_switch_present();

    for (wid, mid, age) in candidates {
        if !be.is_alive(&wid).unwrap_or(false) {
            continue;
        }
        let Some(mol) = inputs.molecules.iter().find(|m| m.id == mid) else {
            continue;
        };
        let Some(cwd) = worker_cwd(inputs.fleet, &wid, inputs.project_root) else {
            out.unattributed.push((wid, mid, 0));
            continue;
        };
        let session_name = mol.session_name.clone().unwrap_or_else(|| wid.to_string());
        if !pane_runs_the_adapter(store, be, &wid, &mid, &session_name) {
            continue;
        }

        out.considered += 1;
        let stall = match journal_verdict(inputs.reg, &cwd) {
            Ok(stall) => stall,
            Err(seen) => {
                out.unattributed.push((wid, mid, seen));
                continue;
            }
        };
        if !stall.flagged {
            out.unflagged += 1;
            continue;
        }

        // §5 no-interference: the flag says the *provider* failed, but a human
        // may already be at the keyboard fixing it. Consulted before any
        // keystroke and before any state write.
        let guard = super::patrol_heal::build_guard_view(
            mol,
            &store.molecule_dir(&mid),
            &presences,
            kill_switched,
            now,
        );
        if let HealGate::Blocked(reason) = heal_gate(&guard, HealthRemedy::Nudge, now, &guard_cfg) {
            out.guarded.push((wid, mid, reason));
            continue;
        }

        let pane_idle =
            pane_idle_seconds(be.socket(), &session_name).map(chrono::Duration::seconds);
        let mut view = build_propulsion_view(
            store,
            inputs.molecules,
            &mid,
            chrono::Duration::seconds(age),
            pane_idle,
        );
        view.channel = NudgeChannel::ApiStall;
        view.api_error_stalled = true;

        let decision = decide_api_stall_nudge(&view, stale_window);
        apply_decision(store, be, &mut out, (wid, mid, age), decision, now);
    }
    out
}

/// The adapter-drift witness `--propel` also emits: is the pane running the
/// program we believe it is?
///
/// The observed `pane_current_command` is compared against a **registry of
/// signatures** and never interpreted — this reads which binary owns the pane,
/// not what that binary printed.
fn pane_runs_the_adapter(
    store: &dyn StateStore,
    be: &TmuxBackend,
    wid: &WorkerId,
    mid: &MoleculeId,
    session_name: &str,
) -> bool {
    let adapters = default_registry();
    let observed = pane_current_command(be.socket(), session_name).unwrap_or_default();
    let matched = adapters.matches(CLAUDE_ADAPTER, &observed);
    emit_adapter_pane_signature_checked(
        &store.molecule_dir(mid),
        mid,
        wid,
        CLAUDE_ADAPTER,
        adapters.signatures_of(CLAUDE_ADAPTER),
        &observed,
        matched,
        PerturbationChannel::Propulsion,
    );
    matched
}

/// File one candidate's verdict — and, for the one verdict that speaks, send
/// the keystroke and record it in the shared propulsion ledger.
fn apply_decision(
    store: &dyn StateStore,
    be: &TmuxBackend,
    out: &mut ApiStallSweep,
    (wid, mid, age): (WorkerId, MoleculeId, i64),
    decision: NudgeDecision,
    now: chrono::DateTime<Utc>,
) {
    match decision {
        NudgeDecision::Skip(NudgeSkip::PaneActive { idle_secs, .. }) => {
            out.sweep.active.push((wid, mid, idle_secs));
        }
        NudgeDecision::Skip(NudgeSkip::AwaitingOperator) => {
            out.sweep.gated.push((wid, mid));
        }
        // `NoTypedApiError` is unreachable — the flag was established before
        // the judge was called — and `NotRunning` cannot fire on a candidate
        // the finder already filtered to Running.
        NudgeDecision::Skip(NudgeSkip::NoTypedApiError | NudgeSkip::NotRunning { .. }) => {}
        NudgeDecision::Skip(NudgeSkip::Backoff {
            since_secs,
            window_secs,
            ..
        }) => {
            out.sweep
                .deferred
                .push((wid, mid, (window_secs - since_secs).max(0)));
        }
        NudgeDecision::Escalate {
            attempts,
            reason: EscalateReason::Orphaned,
        } => {
            escalate_orphan(store, &mut out.sweep, wid, mid, age, attempts);
        }
        NudgeDecision::Escalate {
            attempts,
            reason: EscalateReason::AttemptsExhausted,
        } => {
            mark_propel_exhausted(store, &mid);
            out.sweep.escalated.push((wid, mid, attempts));
        }
        NudgeDecision::Nudge { attempt, .. } => {
            let mol_state_dir = store.molecule_dir(&mid);
            if be
                .send_input_observed(
                    &wid,
                    API_STALL_NUDGE,
                    &cosmon_cli::injection_provenance::propulsion(&mid, &mol_state_dir),
                )
                .is_ok()
            {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = be.send_input_observed(
                    &wid,
                    "",
                    &cosmon_cli::injection_provenance::propulsion_submit(&mid, &mol_state_dir),
                );
                record_propel(store, &mid, attempt, now);
                out.sweep.propelled.push((wid, mid, age));
            }
        }
    }
}

/// The absolute working directory of a worker, as `cs tackle` recorded it.
///
/// `WorkerData::repo` is stored relative to the project root (git's own
/// convention) but legacy fleets carry absolute paths, so resolution goes
/// through [`cosmon_filestore::resolve_repo_path`] rather than a join.
fn worker_cwd(fleet: &Fleet, wid: &WorkerId, project_root: &Path) -> Option<std::path::PathBuf> {
    let repo = fleet.workers.get(wid)?.repo.as_deref()?;
    Some(cosmon_filestore::resolve_repo_path(repo, project_root))
}

// Liveness is deliberately **not** projected by this sweep. `--propel` demotes
// a stale-but-alive worker's process record on its way past; this channel does
// not. An API stall is a fact about the *provider*, not about the worker's
// process, and stamping a health verdict from a transport fault would put a
// wrong fact into a record other organs then read.

/// Human-readable sweep report. Always prints the considered/unflagged pair, so
/// a quiet sweep is legible as "looked, found nothing" rather than as silence.
pub(crate) fn print_api_stall_report(sweep: &ApiStallSweep) {
    use colored::Colorize;

    println!();
    println!("{}", "⚛ API-stall propulsion".bold());
    println!(
        "  {} worker(s) journal-checked, {} with no typed API error",
        sweep.considered, sweep.unflagged
    );

    for (wid, mid, seen) in &sweep.unattributed {
        println!(
            "  {} {} ← {} (journal not attributable: {seen} candidate session(s))",
            "?".yellow(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, reason) in &sweep.guarded {
        println!(
            "  {} {} ← {} (flagged, held by the no-interference guard: {reason:?})",
            "✋".yellow(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, age) in &sweep.sweep.propelled {
        println!(
            "  {} {} ← {} (transport stall, progress frozen {age}s)",
            "→".green(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, idle) in &sweep.sweep.active {
        println!(
            "  {} {} ← {} (flagged but terminal active {idle}s ago — recovering)",
            "·".dimmed(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, remaining) in &sweep.sweep.deferred {
        println!(
            "  {} {} ← {} (backoff, {remaining}s to go)",
            "⏱".dimmed(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid) in &sweep.sweep.gated {
        println!(
            "  {} {} ← {} (operator gate — never nudged)",
            "⏸".yellow(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, attempts) in &sweep.sweep.escalated {
        println!(
            "  {} {} ← {} ({attempts} nudges ignored — tagged `propel-exhausted`)",
            "!".red(),
            wid.as_str(),
            mid.as_str(),
        );
    }
    for (wid, mid, attempts) in &sweep.sweep.orphaned {
        println!(
            "  {} {} ← {} (orphaned, {attempts} prior nudges — tagged `propel-orphaned`)",
            "‼".red(),
            wid.as_str(),
            mid.as_str(),
        );
    }
}

/// The JSON projection of a sweep, for `--json` consumers and the scheduler.
pub(crate) fn api_stall_json(sweep: &ApiStallSweep) -> serde_json::Value {
    let pairs = |v: &[(WorkerId, MoleculeId, i64)]| {
        v.iter()
            .map(|(w, m, n)| serde_json::json!({"worker": w.as_str(), "molecule": m.as_str(), "value": n}))
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "considered": sweep.considered,
        "unflagged": sweep.unflagged,
        "unattributed": sweep
            .unattributed
            .iter()
            .map(|(w, m, seen)| serde_json::json!({
                "worker": w.as_str(), "molecule": m.as_str(), "sessions_seen": seen
            }))
            .collect::<Vec<_>>(),
        "guarded": sweep
            .guarded
            .iter()
            .map(|(w, m, r)| serde_json::json!({
                "worker": w.as_str(), "molecule": m.as_str(), "blocked_by": r
            }))
            .collect::<Vec<_>>(),
        "propelled": pairs(&sweep.sweep.propelled),
        "active": pairs(&sweep.sweep.active),
        "deferred": pairs(&sweep.sweep.deferred),
        "gated": sweep
            .sweep
            .gated
            .iter()
            .map(|(w, m)| serde_json::json!({"worker": w.as_str(), "molecule": m.as_str()}))
            .collect::<Vec<_>>(),
        "escalated": sweep
            .sweep
            .escalated
            .iter()
            .map(|(w, m, a)| serde_json::json!({
                "worker": w.as_str(), "molecule": m.as_str(), "attempts": a
            }))
            .collect::<Vec<_>>(),
        "orphaned": sweep
            .sweep
            .orphaned
            .iter()
            .map(|(w, m, a)| serde_json::json!({
                "worker": w.as_str(), "molecule": m.as_str(), "attempts": a
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_session_probe::ClaudeProbe;
    use std::path::PathBuf;

    /// The sentence from the 2026-08-09 incident. Present in these fixtures as
    /// *text*, never as a matcher — the assertions below turn on the record
    /// type and the typed flag alone.
    const PHRASE: &str = "API Error: Response stalled mid-stream. \
                          The response above may be incomplete.";

    /// Claude sanitises a cwd into its projects/ directory name by mapping
    /// every non-alphanumeric byte to `-`. Mirrored here (and nowhere in the
    /// production path, which reads the `cwd` from inside the log) so a fixture
    /// journal lands where the probe looks for it.
    fn sanitize(path: &str) -> String {
        path.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }

    /// Write one journal for `cwd` under a fake `projects/` root and return a
    /// registry pointed at it.
    fn fixture(root: &Path, cwd: &Path, records: &[String]) -> ProbeRegistry {
        let dir = root.join(sanitize(&cwd.to_string_lossy()));
        std::fs::create_dir_all(&dir).unwrap();
        let envelope = serde_json::json!({
            "type": "user",
            "sessionId": "s1",
            "cwd": cwd.to_string_lossy(),
            "gitBranch": "feat/x",
            "message": {"content": "go"}
        })
        .to_string();
        let mut lines = vec![envelope];
        lines.extend(records.iter().cloned());
        std::fs::write(dir.join("s1.jsonl"), format!("{}\n", lines.join("\n"))).unwrap();
        ProbeRegistry::new().with(Box::new(ClaudeProbe::new(root).unwrap()))
    }

    fn assistant_api_error() -> String {
        serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "timestamp": "2026-08-09T10:05:00Z",
            "message": {"model": "<synthetic>", "content": [{"type": "text", "text": PHRASE}]}
        })
        .to_string()
    }

    fn user_quoting_the_phrase() -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-09T10:06:00Z",
            "message": {"content": format!("earlier I saw: {PHRASE}")}
        })
        .to_string()
    }

    fn assistant_working() -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-09T10:06:30Z",
            "message": {"model": "claude-opus-5", "content": [{"type": "text", "text": "ok"}]}
        })
        .to_string()
    }

    /// **The discriminating pair, half one.** The provider typed the last turn
    /// as a transport failure ⇒ the journal verdict says propel.
    #[test]
    fn a_typed_api_error_in_the_journal_is_a_stall() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/galaxy/.worktrees/task-20260808-3e5c");
        let reg = fixture(tmp.path(), &cwd, &[assistant_api_error()]);
        assert!(journal_verdict(&reg, &cwd).unwrap().flagged);
    }

    /// **The discriminating pair, half two — the be1e case.** The *same
    /// sentence*, this time in a user turn that quotes it, after a healthy
    /// model turn. Nothing is flagged. A mechanism that fails this test is the
    /// one that killed healthy workers.
    #[test]
    fn the_same_sentence_quoted_by_a_user_is_not_a_stall() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/galaxy/.worktrees/task-20260808-3033");
        let reg = fixture(
            tmp.path(),
            &cwd,
            &[assistant_working(), user_quoting_the_phrase()],
        );
        assert!(
            !journal_verdict(&reg, &cwd).unwrap().flagged,
            "quoting the error must never propel a worker"
        );
    }

    /// No journal for that working directory ⇒ refusal, not a guess.
    #[test]
    fn a_worker_with_no_discoverable_journal_is_unattributed() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ProbeRegistry::new().with(Box::new(ClaudeProbe::new(tmp.path()).unwrap()));
        assert_eq!(
            journal_verdict(&reg, Path::new("/galaxy/.worktrees/nobody")).err(),
            Some(0)
        );
    }
}
