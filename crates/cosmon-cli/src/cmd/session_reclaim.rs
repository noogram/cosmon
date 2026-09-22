// SPDX-License-Identifier: AGPL-3.0-only

//! The operator surface over [`cosmon_core::session_reclaim`].
//!
//! `cs purge --sessions` runs the pass in this module. What it does, in the
//! same five movements as the worktree pass it is modelled on:
//!
//! 1. **Enumerate** the tmux socket — `list-panes -a`, not the fleet roster.
//!    The roster is exactly what cannot see this population: the sweep removes
//!    a terminal molecule's worker entry and the session it named goes on
//!    living, unattributable, for as long as the machine is up.
//! 2. **Attribute** each session by computing every known molecule's session
//!    name *forward* through [`cosmon_core::slugify::session_name_for`] and
//!    matching. Never backward: a session name carries four characters of
//!    molecule id, and a four-character match is a guess.
//! 3. **Observe** ownership, status, attachment and scrollback.
//! 4. **Decide** with the pure predicate, and
//! 5. **Report** every session — reclaimed and withheld — each withheld one
//!    with the reason that withheld it.
//!
//! The durable half is carried out of the way before anything is killed: the
//! pane's scrollback is captured to the owning molecule's directory, and a
//! session whose scrollback could not be written is withheld (ADR-179).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use cosmon_core::session_reclaim::{
    consideration_gate, session_selection, AttachmentObservation, Consideration,
    OwnershipObservation, ScrollbackObservation, SessionObservation, SessionSelection,
    StatusObservation,
};
use cosmon_core::worktree_reclaim::ObservationError;
use cosmon_state::{MoleculeFilter, StateStore};

/// One session as tmux reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveSession {
    /// The tmux session name.
    pub name: String,
    /// Axis `C`, already three-valued.
    pub attachment: AttachmentObservation,
}

/// A session the pass selected for reclamation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reclaimed {
    /// The tmux session name.
    pub name: String,
    /// The molecule that owned it.
    pub molecule: String,
    /// That molecule's terminal status, for the report.
    pub status: String,
    /// Where the scrollback went, and how much of it.
    pub scrollback: Option<(PathBuf, usize)>,
}

/// A session the pass declined to touch, and why.
///
/// `reasons` is the load-bearing field, for the same reason it is in the
/// worktree pass: "4 sessions withheld" is not actionable, and
/// "`tmux-session-whose-molecule-e641`: molecule task-20260920-e641 is
/// running" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WithheldSession {
    /// The tmux session name.
    pub name: String,
    /// One line per reason, each naming the thing that decided it.
    pub reasons: Vec<String>,
}

/// Everything one pass found and, if asked, did.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionPass {
    /// Sessions whose reclamation the predicate permitted.
    pub selected: Vec<Reclaimed>,
    /// Sessions withheld, with reasons.
    pub withheld: Vec<WithheldSession>,
    /// Failures during enumeration. Non-empty means the population below is
    /// **incomplete** and must be presented as such.
    pub enumeration_errors: Vec<ObservationError>,
    /// Whether the pass actually killed anything.
    pub executed: bool,
    /// Sessions actually killed, when it did.
    pub killed: Vec<String>,
    /// Per-session kill failures; partial progress is reported, never hidden.
    pub failures: Vec<ObservationError>,
}

// ---------------------------------------------------------------------------
// Enumeration — tmux, not the roster
// ---------------------------------------------------------------------------

/// Ask tmux for every session on the socket and whether a client is attached.
///
/// Deliberately not [`cosmon_core::transport::TransportBackend::list_sessions`]:
/// that one filters out dead panes (rightly — it answers "is this worker
/// alive?") and drops attachment. A carcass is the purest derived state there
/// is, so this pass must see it, and attachment is one of the four axes.
pub(crate) fn enumerate_sessions(socket: &str) -> Result<Vec<LiveSession>, ObservationError> {
    let fail = |cause: String| ObservationError::new("tmux list-panes -a", socket, cause);
    let output = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}|#{session_attached}",
        ])
        .output()
        .map_err(|e| fail(format!("failed to run tmux: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // tmux's several spellings of "there is nothing here" are an empty
        // population, not a failed observation.
        if stderr.contains("no server running")
            || stderr.contains("no sessions")
            || stderr.contains("error connecting")
        {
            return Ok(Vec::new());
        }
        return Err(fail(format!("tmux list-panes failed: {stderr}")));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_pane_listing(&stdout, socket))
}

/// Fold a `#{session_name}|#{session_attached}` listing into one row per
/// session.
///
/// The listing is per *pane*, so a multi-pane session appears repeatedly. A
/// session counts as attached if **any** of its rows says so: a client on one
/// pane is an operator present at the session.
fn parse_pane_listing(stdout: &str, socket: &str) -> Vec<LiveSession> {
    let mut by_name: BTreeMap<String, AttachmentObservation> = BTreeMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, attached)) = line.split_once('|') else {
            continue;
        };
        let observed = match attached.trim() {
            "0" => AttachmentObservation::Detached,
            n if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                AttachmentObservation::Attached
            }
            other => AttachmentObservation::Unknown(ObservationError::new(
                "parse #{session_attached}",
                socket,
                format!("unparseable attachment field {other:?}"),
            )),
        };
        by_name
            .entry(name.trim().to_owned())
            .and_modify(|existing| {
                // Attached wins over Detached; Unknown wins over both, since
                // a row we could not read cannot be overruled by one we could.
                if matches!(observed, AttachmentObservation::Unknown(_))
                    || (matches!(observed, AttachmentObservation::Attached)
                        && matches!(existing, AttachmentObservation::Detached))
                {
                    *existing = observed.clone();
                }
            })
            .or_insert(observed);
    }
    by_name
        .into_iter()
        .map(|(name, attachment)| LiveSession { name, attachment })
        .collect()
}

/// The session this very command is running inside, if any.
///
/// Defence in depth, and nothing more: a molecule whose worker is running this
/// command is `Running`, so the gate already withholds its session. But this
/// pass kills processes, and a second, independent reason not to kill our own
/// pane is worth the four lines it costs.
pub(crate) fn current_session(socket: &str) -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let output = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(["display-message", "-p", "-t", &pane, "#{session_name}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

// ---------------------------------------------------------------------------
// Attribution — forward, never backward
// ---------------------------------------------------------------------------

/// What a molecule contributes to the ownership index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Owner {
    /// The owning molecule's id.
    pub molecule: String,
    /// Its status, already read.
    pub status: cosmon_core::molecule::MoleculeStatus,
}

/// Build session-name → owning molecule by computing each molecule's session
/// name forward.
///
/// This is the same mapping `cs patrol` uses to spot unrecorded dispatches,
/// and it is the only direction that is sound. It survives the roster entry's
/// removal, which is what lets this pass see the sessions the sweep orphaned.
pub(crate) fn ownership_index(
    store: &dyn StateStore,
) -> Result<BTreeMap<String, Owner>, ObservationError> {
    let molecules = store
        .list_molecules(&MoleculeFilter::default())
        .map_err(|e| ObservationError::new("list_molecules", ".cosmon/state", e.to_string()))?;
    let mut index = BTreeMap::new();
    for m in molecules {
        let name = cosmon_core::slugify::session_name_for(m.display_topic(), m.id.as_str());
        index.insert(
            name,
            Owner {
                molecule: m.id.as_str().to_owned(),
                status: m.status,
            },
        );
    }
    Ok(index)
}

// ---------------------------------------------------------------------------
// The durable half — capture before kill
// ---------------------------------------------------------------------------

/// Where a session's captured scrollback is written.
fn scrollback_path(store: &dyn StateStore, molecule: &str, session: &str) -> Option<PathBuf> {
    let id = cosmon_core::id::MoleculeId::new(molecule).ok()?;
    let dir = store.molecule_dir(&id);
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join(format!("session-scrollback-{session}.txt")))
}

/// Capture the full scrollback of **every** pane in a session.
///
/// Addressed by pane id (`%0`), which is the only unambiguous target. The
/// session-scoped spellings are each wrong in their own way: `-t =<session>`
/// is session-target syntax that `capture-pane` does not accept at all, and
/// `-t <session>:` silently resolves to the active pane of the current window,
/// so a split or a background window would be captured as nothing and the
/// operator would never learn which part they lost.
fn capture_scrollback(socket: &str, session: &str) -> Result<String, ObservationError> {
    let fail = |cause: String| ObservationError::new("tmux capture-pane", socket, cause);

    let listing = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(["list-panes", "-t", session, "-F", "#{pane_id}"])
        .output()
        .map_err(|e| fail(format!("failed to run tmux: {e}")))?;
    if !listing.status.success() {
        let stderr = String::from_utf8_lossy(&listing.stderr);
        return Err(fail(format!("list-panes for {session} failed: {stderr}")));
    }
    let pane_ids: Vec<String> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if pane_ids.is_empty() {
        return Err(fail(format!("{session} reported no panes to capture")));
    }

    let mut captured = String::new();
    for pane in &pane_ids {
        let output = Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .args(["capture-pane", "-p", "-S", "-", "-t", pane])
            .output()
            .map_err(|e| fail(format!("failed to run tmux: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // One unreadable pane fails the whole capture. A partial
            // scrollback presented as the scrollback is the failure mode this
            // axis exists to prevent.
            return Err(fail(format!(
                "capture-pane {pane} of {session} failed: {stderr}"
            )));
        }
        if pane_ids.len() > 1 {
            // A multi-pane capture must say where each part came from, or the
            // file reads as one stream that never existed.
            let _ = writeln!(captured, "===== pane {pane} =====");
        }
        captured.push_str(&String::from_utf8_lossy(&output.stdout));
        if !captured.ends_with('\n') {
            captured.push('\n');
        }
    }
    Ok(captured)
}

/// Observe the scrollback axis.
///
/// In `execute` mode this captures **and writes**, so `Captured` means the
/// bytes are on disk. In dry-run it captures and discards, so `Captured` means
/// the capture works and the real run would write it. The two modes differ in
/// what they leave behind and agree on the verdict, which is the only thing a
/// preview has to promise.
fn observe_scrollback(
    socket: &str,
    session: &str,
    dest: Option<&PathBuf>,
    execute: bool,
) -> ScrollbackObservation {
    let captured = match capture_scrollback(socket, session) {
        Ok(text) => text,
        Err(e) => return ScrollbackObservation::Unavailable(e),
    };
    let lines = captured.lines().count();
    let Some(dest) = dest else {
        return ScrollbackObservation::Unavailable(ObservationError::new(
            "resolve molecule_dir",
            session,
            "the owning molecule has no on-disk directory to hold its scrollback".to_owned(),
        ));
    };
    if !execute {
        return ScrollbackObservation::Captured { lines };
    }
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ScrollbackObservation::Unavailable(ObservationError::new(
                "create_dir_all",
                parent,
                e.to_string(),
            ));
        }
    }
    match std::fs::write(dest, captured.as_bytes()) {
        Ok(()) => ScrollbackObservation::Captured { lines },
        Err(e) => ScrollbackObservation::Unavailable(ObservationError::new(
            "write scrollback",
            dest,
            e.to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Reasons
// ---------------------------------------------------------------------------

/// Why this session was withheld — one line per failing axis, or `None` when
/// nothing withheld it.
pub(crate) fn withhold_reasons(obs: &SessionObservation) -> Vec<String> {
    let mut reasons = Vec::new();
    match (&obs.ownership, &obs.status) {
        (OwnershipObservation::Unowned, _) => reasons.push(
            "no molecule claims this session name — cosmon does not own it, and a \
             tmux socket is a shared namespace"
                .to_owned(),
        ),
        (OwnershipObservation::Unknown(e), _) => {
            reasons.push(format!(
                "ownership could not be established: {}",
                e.describe()
            ));
        }
        (OwnershipObservation::Owned(id), StatusObservation::Unknown(e)) => {
            reasons.push(format!("status of {id} is unreadable: {}", e.describe()));
        }
        (OwnershipObservation::Owned(id), StatusObservation::Known(s)) if !s.is_terminal() => {
            reasons.push(format!(
                "molecule {id} is {} — a session is derived state only once its \
                 molecule is terminal",
                s.as_str()
            ));
        }
        (OwnershipObservation::Owned(_), StatusObservation::Known(_)) => {}
    }
    match &obs.attachment {
        AttachmentObservation::Attached => {
            reasons.push("a tmux client is attached — an operator is at this pane".to_owned());
        }
        AttachmentObservation::Unknown(e) => {
            reasons.push(format!("attachment is unknown: {}", e.describe()));
        }
        AttachmentObservation::Detached => {}
    }
    // The scrollback only speaks once the gate permits: reporting "scrollback
    // not captured" for a running molecule's session would name a consequence
    // of the real reason as though it were one.
    if consideration_gate(&obs.ownership, &obs.status) == Consideration::Yes {
        if let ScrollbackObservation::Unavailable(e) = &obs.scrollback {
            reasons.push(format!(
                "scrollback could not be secured, so the session stays up: {}",
                e.describe()
            ));
        }
    }
    reasons
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// Run one session-reclaim pass.
///
/// `execute` is the operator's ask. Without it nothing is captured to disk and
/// nothing is killed; the verdicts are identical either way.
pub(crate) fn run_pass(socket: &str, store: &dyn StateStore, execute: bool) -> SessionPass {
    let mut pass = SessionPass {
        executed: execute,
        ..SessionPass::default()
    };

    let sessions = match enumerate_sessions(socket) {
        Ok(s) => s,
        Err(e) => {
            pass.enumeration_errors.push(e);
            return pass;
        }
    };

    // An index we could not build is not an empty index: every session would
    // read as Unowned and the pass would silently report "nothing to do".
    let index = match ownership_index(store) {
        Ok(i) => Some(i),
        Err(e) => {
            pass.enumeration_errors.push(e);
            None
        }
    };

    let self_session = current_session(socket);

    for live in sessions {
        if self_session.as_deref() == Some(live.name.as_str()) {
            pass.withheld.push(WithheldSession {
                name: live.name,
                reasons: vec![
                    "this is the session `cs purge` is running in — never reclaimed".to_owned(),
                ],
            });
            continue;
        }

        let (obs, dest) = observe_session(socket, store, index.as_ref(), live, execute);
        match session_selection(&obs) {
            SessionSelection::Keep => pass.withheld.push(WithheldSession {
                name: obs.name.clone(),
                reasons: withhold_reasons(&obs),
            }),
            SessionSelection::ReclaimSession => pass.selected.push(reclaimed_from(&obs, dest)),
        }
    }

    if execute {
        for target in pass.selected.clone() {
            if let Err(e) = kill_session(socket, &target.name) {
                pass.failures.push(e);
            } else {
                pass.killed.push(target.name);
            }
        }
    }

    pass.withheld.sort_by(|a, b| a.name.cmp(&b.name));
    pass.selected.sort_by(|a, b| a.name.cmp(&b.name));
    pass
}

/// Fill every axis for one live session.
///
/// Returns the observation and the scrollback destination alongside it, so the
/// caller can report where the bytes went without recomputing the path.
fn observe_session(
    socket: &str,
    store: &dyn StateStore,
    index: Option<&BTreeMap<String, Owner>>,
    live: LiveSession,
    execute: bool,
) -> (SessionObservation, Option<PathBuf>) {
    let (ownership, status) = match index {
        None => (
            OwnershipObservation::Unknown(ObservationError::new(
                "list_molecules",
                ".cosmon/state",
                "the molecule enumeration failed; ownership is unknown".to_owned(),
            )),
            StatusObservation::Unknown(ObservationError::new(
                "list_molecules",
                ".cosmon/state",
                "no molecule could be read".to_owned(),
            )),
        ),
        Some(index) => match index.get(&live.name) {
            None => (
                OwnershipObservation::Unowned,
                StatusObservation::Unknown(ObservationError::new(
                    "resolve owner",
                    &live.name,
                    "no owning molecule, so no status to read".to_owned(),
                )),
            ),
            Some(owner) => (
                OwnershipObservation::Owned(owner.molecule.clone()),
                StatusObservation::Known(owner.status),
            ),
        },
    };

    let dest = match &ownership {
        OwnershipObservation::Owned(id) => scrollback_path(store, id, &live.name),
        OwnershipObservation::Unowned | OwnershipObservation::Unknown(_) => None,
    };

    // Capture only where the gate already permits. Capturing every live
    // session's scrollback on every purge would be a side effect nobody asked
    // for, and in execute mode it would write a file into the directory of a
    // molecule that is still running.
    let scrollback = if consideration_gate(&ownership, &status) == Consideration::Yes {
        observe_scrollback(socket, &live.name, dest.as_ref(), execute)
    } else {
        ScrollbackObservation::Unavailable(ObservationError::new(
            "capture-pane",
            &live.name,
            "not attempted: the gate withheld this session".to_owned(),
        ))
    };

    (
        SessionObservation {
            name: live.name,
            ownership,
            status,
            attachment: live.attachment,
            scrollback,
        },
        dest,
    )
}

/// Build the report row for a session the predicate selected.
fn reclaimed_from(obs: &SessionObservation, dest: Option<PathBuf>) -> Reclaimed {
    let (molecule, status) = match (&obs.ownership, &obs.status) {
        (OwnershipObservation::Owned(id), StatusObservation::Known(s)) => {
            (id.clone(), s.as_str().to_owned())
        }
        // Unreachable: the predicate returned ReclaimSession, so the gate
        // passed, so both are positive. Named rather than unwrapped, per the
        // project's no-`expect()` rule.
        _ => (String::from("<unknown>"), String::from("<unknown>")),
    };
    let lines = match obs.scrollback {
        ScrollbackObservation::Captured { lines } => lines,
        ScrollbackObservation::Unavailable(_) => 0,
    };
    Reclaimed {
        name: obs.name.clone(),
        molecule,
        status,
        scrollback: dest.map(|d| (d, lines)),
    }
}

/// Kill one session by exact name.
fn kill_session(socket: &str, session: &str) -> Result<(), ObservationError> {
    let output = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(["kill-session", "-t", &format!("={session}")])
        .output()
        .map_err(|e| {
            ObservationError::new(
                "tmux kill-session",
                session,
                format!("failed to run tmux: {e}"),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A session that vanished between the decision and the kill is the
    // outcome we wanted, reached by someone else.
    if stderr.contains("can't find session") || stderr.contains("session not found") {
        return Ok(());
    }
    Err(ObservationError::new(
        "tmux kill-session",
        session,
        stderr.trim().to_owned(),
    ))
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// What mode the pass ran in, for the report's first line.
fn mode_label(pass: &SessionPass) -> &'static str {
    if pass.executed {
        "reclaimed"
    } else {
        "would reclaim (dry)"
    }
}

/// Print the operator report.
pub(crate) fn report(pass: &SessionPass) {
    println!("\ntmux session reclamation:");

    if !pass.enumeration_errors.is_empty() {
        println!("  ⚠ enumeration incomplete — the population below is NOT the whole one:");
        for e in &pass.enumeration_errors {
            println!("    - {}", e.describe());
        }
    }

    if pass.selected.is_empty() {
        println!("  {}: none.", mode_label(pass));
    } else {
        println!("  {} {} session(s):", mode_label(pass), pass.selected.len());
        for s in &pass.selected {
            let scroll = match &s.scrollback {
                Some((path, lines)) => {
                    format!(" — {lines} scrollback line(s) → {}", path.display())
                }
                None => String::new(),
            };
            println!(
                "    - {} (molecule {} {}){scroll}",
                s.name, s.molecule, s.status
            );
        }
    }

    if !pass.withheld.is_empty() {
        println!("  withheld {} session(s):", pass.withheld.len());
        for w in &pass.withheld {
            println!("    - {}", w.name);
            for r in &w.reasons {
                println!("        {r}");
            }
        }
    }

    if !pass.failures.is_empty() {
        println!("  {} kill failure(s):", pass.failures.len());
        for e in &pass.failures {
            println!("    - {}", e.describe());
        }
    }

    if !pass.executed && !pass.selected.is_empty() {
        println!(
            "  (dry — nothing was captured or killed. Re-run with \
             `--allow-unharvested` to execute.)"
        );
    }
}

/// The `--json` shape of a pass.
pub(crate) fn to_json(pass: &SessionPass) -> serde_json::Value {
    serde_json::json!({
        "command": "purge",
        "pass": "sessions",
        "executed": pass.executed,
        "selected": pass.selected.iter().map(|s| serde_json::json!({
            "session": s.name,
            "molecule": s.molecule,
            "status": s.status,
            "scrollback_path": s.scrollback.as_ref().map(|(p, _)| p.display().to_string()),
            "scrollback_lines": s.scrollback.as_ref().map(|(_, l)| *l),
        })).collect::<Vec<_>>(),
        "withheld": pass.withheld.iter().map(|w| serde_json::json!({
            "session": w.name,
            "reasons": w.reasons,
        })).collect::<Vec<_>>(),
        "killed": pass.killed,
        "enumeration_errors": pass.enumeration_errors.iter()
            .map(ObservationError::describe).collect::<Vec<_>>(),
        "failures": pass.failures.iter()
            .map(ObservationError::describe).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::molecule::MoleculeStatus;

    fn err() -> ObservationError {
        ObservationError::new("op", "/p", "cause")
    }

    #[test]
    fn a_pane_listing_folds_to_one_row_per_session() {
        let rows = parse_pane_listing("a|0\nb|1\na|0\n", "sock");
        assert_eq!(
            rows,
            vec![
                LiveSession {
                    name: "a".into(),
                    attachment: AttachmentObservation::Detached
                },
                LiveSession {
                    name: "b".into(),
                    attachment: AttachmentObservation::Attached
                },
            ]
        );
    }

    /// A client on any pane is an operator at the session. The fold must not
    /// let a later detached pane overwrite an earlier attached one.
    #[test]
    fn any_attached_pane_makes_the_whole_session_attached() {
        for listing in ["s|1\ns|0\n", "s|0\ns|1\n"] {
            let rows = parse_pane_listing(listing, "sock");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].attachment,
                AttachmentObservation::Attached,
                "{listing:?}"
            );
        }
    }

    /// An unparseable field is `Unknown` and outranks both readable values:
    /// a row we could not read cannot be overruled by one we could.
    #[test]
    fn an_unparseable_attachment_field_is_unknown_and_wins() {
        let rows = parse_pane_listing("s|0\ns|banana\n", "sock");
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0].attachment,
            AttachmentObservation::Unknown(_)
        ));
    }

    #[test]
    fn an_empty_listing_is_an_empty_population_not_an_error() {
        assert!(parse_pane_listing("", "sock").is_empty());
        assert!(parse_pane_listing("\n  \n", "sock").is_empty());
    }

    fn obs(
        ownership: OwnershipObservation,
        status: StatusObservation,
        attachment: AttachmentObservation,
        scrollback: ScrollbackObservation,
    ) -> SessionObservation {
        SessionObservation {
            name: "s".into(),
            ownership,
            status,
            attachment,
            scrollback,
        }
    }

    #[test]
    fn a_permitted_session_has_no_withhold_reasons() {
        let o = obs(
            OwnershipObservation::Owned("m".into()),
            StatusObservation::Known(MoleculeStatus::Completed),
            AttachmentObservation::Detached,
            ScrollbackObservation::Captured { lines: 3 },
        );
        assert!(withhold_reasons(&o).is_empty());
        assert_eq!(session_selection(&o), SessionSelection::ReclaimSession);
    }

    /// Every axis that can withhold names itself, so an operator reading the
    /// register learns what to do rather than that a number went up.
    #[test]
    fn every_withholding_axis_names_its_own_reason() {
        let cases: Vec<(SessionObservation, &str)> = vec![
            (
                obs(
                    OwnershipObservation::Unowned,
                    StatusObservation::Unknown(err()),
                    AttachmentObservation::Detached,
                    ScrollbackObservation::Captured { lines: 1 },
                ),
                "does not own it",
            ),
            (
                obs(
                    OwnershipObservation::Unknown(err()),
                    StatusObservation::Unknown(err()),
                    AttachmentObservation::Detached,
                    ScrollbackObservation::Captured { lines: 1 },
                ),
                "ownership could not be established",
            ),
            (
                obs(
                    OwnershipObservation::Owned("m".into()),
                    StatusObservation::Known(MoleculeStatus::Running),
                    AttachmentObservation::Detached,
                    ScrollbackObservation::Captured { lines: 1 },
                ),
                "is running",
            ),
            (
                obs(
                    OwnershipObservation::Owned("m".into()),
                    StatusObservation::Unknown(err()),
                    AttachmentObservation::Detached,
                    ScrollbackObservation::Captured { lines: 1 },
                ),
                "unreadable",
            ),
            (
                obs(
                    OwnershipObservation::Owned("m".into()),
                    StatusObservation::Known(MoleculeStatus::Completed),
                    AttachmentObservation::Attached,
                    ScrollbackObservation::Captured { lines: 1 },
                ),
                "client is attached",
            ),
            (
                obs(
                    OwnershipObservation::Owned("m".into()),
                    StatusObservation::Known(MoleculeStatus::Completed),
                    AttachmentObservation::Detached,
                    ScrollbackObservation::Unavailable(err()),
                ),
                "scrollback could not be secured",
            ),
        ];
        for (o, needle) in cases {
            let reasons = withhold_reasons(&o);
            assert_eq!(session_selection(&o), SessionSelection::Keep);
            assert!(
                reasons.iter().any(|r| r.contains(needle)),
                "expected a reason containing {needle:?}, got {reasons:?}"
            );
        }
    }

    /// A withheld session must be told why it was *really* withheld. The
    /// scrollback is not attempted behind a closed gate, so naming its absence
    /// would report a consequence of the real reason as though it were one.
    #[test]
    fn a_vetoed_gate_does_not_also_blame_the_scrollback() {
        let o = obs(
            OwnershipObservation::Owned("m".into()),
            StatusObservation::Known(MoleculeStatus::Running),
            AttachmentObservation::Detached,
            ScrollbackObservation::Unavailable(err()),
        );
        let reasons = withhold_reasons(&o);
        assert!(reasons.iter().any(|r| r.contains("is running")));
        assert!(
            !reasons.iter().any(|r| r.contains("scrollback")),
            "gate-vetoed session must not be blamed for an unattempted capture: {reasons:?}"
        );
    }
}
