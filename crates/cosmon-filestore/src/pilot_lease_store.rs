// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem backend for the PRIMARY lease (mission co-pilotage M4).
//!
//! Two append-only NDJSON files per mission, both `jq`-readable, both under
//! `.cosmon/state/`:
//!
//! ```text
//! pilot-lease/<mission>.requests.jsonl   one LeaseRequest per line — pilots write
//! pilot-lease/<mission>.grants.jsonl     one PilotLease per line   — the operator writes
//! ```
//!
//! # Why two files and not one
//!
//! Because they have different writers, and a file with one writer cannot be
//! corrupted by a race it does not have. A pilot may append an ask at any time;
//! only an operator gesture appends a grant. Keeping the ask out of the
//! authority ledger is also what makes the M4 crash clause trivially true: a
//! process killed between `request` and `grant` has written a line to the
//! requests file and nothing to the grants file, so [`PilotLeaseStore::current`]
//! returns exactly what it returned before. There is no half-transfer state
//! because a transfer is one append.
//!
//! # Why append-only and not a single mutable `lease.json`
//!
//! An overwrite has two failure modes a co-pilot cannot distinguish: a
//! truncated write that loses the prior holder, and a transfer that never
//! happened. An append has neither, and it leaves the epoch history on disk —
//! which is the M4 acceptance clause on *state inspectable and recovery
//! demonstrated*. `cat` the grants file and the whole authority history of the
//! mission is in front of you, oldest first.
//!
//! # The one rule the store enforces
//!
//! An epoch is used once. [`PilotLeaseStore::grant`] refuses a grant whose
//! epoch is not strictly greater than every epoch already in the ledger, so
//! two operators racing on one mission produce one winner and one refusal
//! rather than two heads (NO-SPLIT-BRAIN).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::error::CosmonError;
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::paths::CosmonPath;
use cosmon_core::pilot_lease::{
    authorize, LeaseDecision, LeaseEpoch, LeaseRequest, PilotLease, RequestId,
};

/// File-backed lease ledger. Stateless; every call is a pure function of the
/// on-disk layout, exactly like [`crate::PilotMailbox`].
#[derive(Debug, Clone)]
pub struct PilotLeaseStore {
    /// The cosmon **state root** (`.cosmon/state/`).
    state_root: PathBuf,
}

impl PilotLeaseStore {
    /// Construct a lease store over the given cosmon state root.
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    /// Path of a mission's request file.
    #[must_use]
    pub fn requests_path(&self, mission: &MoleculeId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PilotLeaseRequests { mission }.rel())
    }

    /// Path of a mission's grant ledger.
    #[must_use]
    pub fn grants_path(&self, mission: &MoleculeId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PilotLeaseGrants { mission }.rel())
    }

    /// Every grant recorded for `mission`, in file order (oldest first).
    ///
    /// A line that fails to parse is skipped rather than fatal — one torn
    /// trailing line from a process killed mid-append must not make a
    /// mission's authority unreadable. It cannot manufacture authority
    /// either: a skipped line is a grant that did not happen.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] if the file exists but cannot be
    /// read. A missing file is an empty ledger, not an error.
    pub fn grants(&self, mission: &MoleculeId) -> Result<Vec<PilotLease>, CosmonError> {
        Ok(read_lines(&self.grants_path(mission))?
            .iter()
            .filter_map(|l| serde_json::from_str::<PilotLease>(l).ok())
            .collect())
    }

    /// Every request recorded for `mission`, in file order.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`].
    pub fn requests(&self, mission: &MoleculeId) -> Result<Vec<LeaseRequest>, CosmonError> {
        Ok(read_lines(&self.requests_path(mission))?
            .iter()
            .filter_map(|l| serde_json::from_str::<LeaseRequest>(l).ok())
            .collect())
    }

    /// The lease in force: the grant with the highest epoch, or `None` if the
    /// mission has never been granted.
    ///
    /// Highest epoch rather than last line, because the two disagree exactly
    /// when something went wrong — a concurrent append, a partially ordered
    /// filesystem — and in that case the ledger's own ordering rule is the one
    /// that should win.
    ///
    /// Expiry is **not** applied here: `current` answers "what does the ledger
    /// say", and [`Self::authorize`] answers "may this session act now". A
    /// caller that conflated them would render an expired lease as no lease
    /// and lose the reason.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`].
    pub fn current(&self, mission: &MoleculeId) -> Result<Option<PilotLease>, CosmonError> {
        Ok(self.grants(mission)?.into_iter().max_by_key(|l| l.epoch))
    }

    /// The epoch a transfer should be granted at: one past the highest in the
    /// ledger, or [`LeaseEpoch::first`] on a mission with no grants.
    ///
    /// Derived from the file rather than kept in a counter, because a counter
    /// is a second source of truth that a crash can desynchronise.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`].
    pub fn next_epoch(&self, mission: &MoleculeId) -> Result<LeaseEpoch, CosmonError> {
        Ok(self
            .grants(mission)?
            .iter()
            .map(|l| l.epoch)
            .max()
            .map_or_else(LeaseEpoch::first, LeaseEpoch::next))
    }

    /// Append a takeover ask unless one with the same id is already recorded.
    ///
    /// Returns `true` if the line was written, `false` if the request was
    /// already there — so a retry after a crash reports "already asked"
    /// instead of queueing a second ask the operator would have to arbitrate.
    ///
    /// This **grants nothing**. There is no path from here to a lease.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] on a filesystem failure.
    pub fn request(&self, request: &LeaseRequest) -> Result<bool, CosmonError> {
        if self
            .requests(&request.mission_id)?
            .iter()
            .any(|r| r.id == request.id)
        {
            return Ok(false);
        }
        let line = serde_json::to_string(request).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to serialise lease request: {e}"),
        })?;
        append_line(&self.requests_path(&request.mission_id), &line)?;
        Ok(true)
    }

    /// Find one recorded request by id.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`].
    pub fn find_request(
        &self,
        mission: &MoleculeId,
        id: &RequestId,
    ) -> Result<Option<LeaseRequest>, CosmonError> {
        Ok(self.requests(mission)?.into_iter().find(|r| &r.id == id))
    }

    /// Requests that no grant has answered yet.
    ///
    /// A request stays listed after a grant that did not name it: the operator
    /// declining to answer an ask is not the same fact as the ask never having
    /// been made, and only one of the two is worth showing.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`].
    pub fn unanswered_requests(
        &self,
        mission: &MoleculeId,
    ) -> Result<Vec<LeaseRequest>, CosmonError> {
        let answered: Vec<RequestId> = self
            .grants(mission)?
            .into_iter()
            .filter_map(|l| l.request_id)
            .collect();
        Ok(self
            .requests(mission)?
            .into_iter()
            .filter(|r| !answered.contains(&r.id))
            .collect())
    }

    /// Append a grant to the mission's authority ledger.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] when the grant's epoch is not
    /// strictly greater than every epoch already recorded — a reused epoch is
    /// a second head, and refusing it here is what makes NO-SPLIT-BRAIN a
    /// property of the store rather than of the caller's care. Also on a
    /// filesystem failure.
    pub fn grant(&self, lease: &PilotLease) -> Result<(), CosmonError> {
        let highest = self
            .grants(&lease.mission_id)?
            .iter()
            .map(|l| l.epoch)
            .max();
        if let Some(highest) = highest {
            if lease.epoch <= highest {
                return Err(CosmonError::StateStore {
                    reason: format!(
                        "refusing a grant at epoch {} — mission {} is already at epoch {}; \
                         an epoch is used once",
                        lease.epoch,
                        lease.mission_id.as_str(),
                        highest,
                    ),
                });
            }
        }
        let line = serde_json::to_string(lease).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to serialise lease: {e}"),
        })?;
        append_line(&self.grants_path(&lease.mission_id), &line)
    }

    /// The guard: may `session`, believing it holds `presented_epoch`, issue a
    /// piloting gesture on `mission` as of `now`?
    ///
    /// Reads the ledger and delegates the decision to the I/O-free
    /// [`authorize`]. Every refusal path in that function is reachable from
    /// here, and no new one is introduced: the store adds no policy.
    ///
    /// # Errors
    ///
    /// As [`Self::grants`]. An unreadable ledger is an error and **not** a
    /// refusal — "I could not check" and "I checked and the answer is no" are
    /// different facts, and a caller that treated them alike would eventually
    /// treat one as the other in the permissive direction.
    pub fn authorize(
        &self,
        mission: &MoleculeId,
        now: DateTime<Utc>,
        session: &SessionId,
        presented_epoch: Option<LeaseEpoch>,
    ) -> Result<LeaseDecision, CosmonError> {
        let current = self.current(mission)?;
        Ok(authorize(current.as_ref(), now, session, presented_epoch))
    }
}

/// Read a file into lines, treating "does not exist" as "empty".
fn read_lines(path: &PathBuf) -> Result<Vec<String>, CosmonError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s.lines().map(str::to_owned).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(CosmonError::StateStore {
            reason: format!("failed to read {}: {e}", path.display()),
        }),
    }
}

/// Append one newline-terminated line, creating the parent directory on first
/// write.
fn append_line(path: &PathBuf, line: &str) -> Result<(), CosmonError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CosmonError::StateStore {
            reason: format!("failed to open {}: {e}", path.display()),
        })?;
    writeln!(f, "{line}").map_err(|e| CosmonError::StateStore {
        reason: format!("failed to append to {}: {e}", path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use cosmon_core::pilot_lease::RefusalReason;
    use tempfile::tempdir;

    fn mission() -> MoleculeId {
        MoleculeId::new("task-20260731-9cf4").unwrap()
    }

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 9, 0, 0).unwrap()
    }

    fn store() -> (tempfile::TempDir, PilotLeaseStore) {
        let dir = tempdir().unwrap();
        let store = PilotLeaseStore::new(dir.path());
        (dir, store)
    }

    fn lease(holder: &str, epoch: LeaseEpoch) -> PilotLease {
        PilotLease::new(mission(), sid(holder), epoch, "operator", t0(), None)
    }

    // FAIL-CLOSED-AUTHORITY on a mission nobody has ever granted.
    #[test]
    fn a_cold_mission_has_no_lease_and_authorises_nobody() {
        let (_d, s) = store();
        assert!(s.current(&mission()).unwrap().is_none());
        assert_eq!(s.next_epoch(&mission()).unwrap(), LeaseEpoch::first());
        let d = s
            .authorize(&mission(), t0(), &sid("claude"), Some(LeaseEpoch::first()))
            .unwrap();
        assert_eq!(d, LeaseDecision::Refused(RefusalReason::NoLease));
    }

    #[test]
    fn a_grant_becomes_the_current_lease() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        let cur = s.current(&mission()).unwrap().unwrap();
        assert_eq!(cur.holder_session_id, sid("claude"));
        assert_eq!(cur.epoch, LeaseEpoch::first());
        assert_eq!(
            s.next_epoch(&mission()).unwrap(),
            LeaseEpoch::first().next()
        );
    }

    // PRIMARY-UNIQUE / NO-SPLIT-BRAIN: an epoch is used once. Two operators
    // racing produce one winner and one refusal, not two heads.
    #[test]
    fn a_reused_epoch_is_refused_by_the_ledger() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        let err = s
            .grant(&lease("codex", LeaseEpoch::first()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("epoch is used once"), "{err}");
        // And the ledger is unchanged — the refusal wrote nothing.
        assert_eq!(s.grants(&mission()).unwrap().len(), 1);
        assert_eq!(
            s.current(&mission()).unwrap().unwrap().holder_session_id,
            sid("claude")
        );
    }

    #[test]
    fn a_grant_that_goes_backwards_is_refused_too() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::new(5).unwrap()))
            .unwrap();
        assert!(s
            .grant(&lease("codex", LeaseEpoch::new(3).unwrap()))
            .is_err());
    }

    // The two headline acceptance clauses of M4, on the real ledger.
    #[test]
    fn a_transfer_moves_authority_and_the_former_primary_is_refused() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        assert!(s
            .authorize(&mission(), t0(), &sid("claude"), Some(LeaseEpoch::first()))
            .unwrap()
            .is_granted());
        // The co-pilot cannot act while the primary holds it.
        assert!(!s
            .authorize(&mission(), t0(), &sid("codex"), Some(LeaseEpoch::first()))
            .unwrap()
            .is_granted());

        // The operator transfers.
        let next = s.next_epoch(&mission()).unwrap();
        s.grant(&lease("codex", next)).unwrap();

        // The former primary is refused, still presenting the epoch it held.
        let d = s
            .authorize(&mission(), t0(), &sid("claude"), Some(LeaseEpoch::first()))
            .unwrap();
        assert_eq!(
            d,
            LeaseDecision::Refused(RefusalReason::NotHolder {
                holder: sid("codex")
            })
        );
        // The new primary is authorised at the new epoch.
        assert!(s
            .authorize(&mission(), t0(), &sid("codex"), Some(next))
            .unwrap()
            .is_granted());
    }

    // "crash entre request et grant sans changement d'autorité": the ask is
    // written, the grant is not, and the answer to "who is PRIMARY" is
    // byte-for-byte what it was.
    #[test]
    fn a_crash_between_request_and_grant_changes_no_authority() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        let before = s.current(&mission()).unwrap();

        let held = before.clone().unwrap();
        let req = LeaseRequest::new(
            mission(),
            sid("codex"),
            sid("codex"),
            Some(&held),
            t0(),
            "claude is near its window limit",
        );
        assert!(s.request(&req).unwrap());
        // …and the process dies here, before the operator grants anything.

        assert_eq!(s.current(&mission()).unwrap(), before);
        assert!(!s
            .authorize(&mission(), t0(), &sid("codex"), Some(LeaseEpoch::first()))
            .unwrap()
            .is_granted());
        assert!(s
            .authorize(&mission(), t0(), &sid("claude"), Some(LeaseEpoch::first()))
            .unwrap()
            .is_granted());
    }

    #[test]
    fn a_retried_request_is_recorded_once() {
        let (_d, s) = store();
        let req = LeaseRequest::new(mission(), sid("codex"), sid("codex"), None, t0(), "");
        assert!(s.request(&req).unwrap(), "first ask is written");
        assert!(!s.request(&req).unwrap(), "the retry is a no-op");
        assert_eq!(s.requests(&mission()).unwrap().len(), 1);
    }

    #[test]
    fn a_request_is_unanswered_until_a_grant_names_it() {
        let (_d, s) = store();
        let req = LeaseRequest::new(mission(), sid("codex"), sid("codex"), None, t0(), "");
        s.request(&req).unwrap();
        assert_eq!(s.unanswered_requests(&mission()).unwrap().len(), 1);
        assert_eq!(s.find_request(&mission(), &req.id).unwrap().unwrap(), req);

        // A grant that does not name it leaves it listed.
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        assert_eq!(s.unanswered_requests(&mission()).unwrap().len(), 1);

        // The grant that answers it clears it.
        let next = s.next_epoch(&mission()).unwrap();
        s.grant(&lease("codex", next).answering(&req)).unwrap();
        assert!(s.unanswered_requests(&mission()).unwrap().is_empty());
    }

    #[test]
    fn expiry_refuses_without_erasing_the_ledger() {
        let (_d, s) = store();
        let deadline = t0() + Duration::minutes(30);
        let mut l = lease("claude", LeaseEpoch::first());
        l.expires_at = Some(deadline);
        s.grant(&l).unwrap();

        let late = deadline + Duration::minutes(1);
        assert!(matches!(
            s.authorize(&mission(), late, &sid("claude"), Some(LeaseEpoch::first()))
                .unwrap(),
            LeaseDecision::Refused(RefusalReason::Expired { .. })
        ));
        // `current` still reports the record: an expired lease is a fact about
        // the past, not an absent one.
        assert!(s.current(&mission()).unwrap().is_some());
    }

    // Recovery: the whole authority history survives on disk and the head is
    // recomputed from it, so a reader that lost all memory reconstructs the
    // same answer.
    #[test]
    fn the_head_is_recomputed_from_the_ledger_not_remembered() {
        let (dir, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        s.grant(&lease("codex", LeaseEpoch::new(2).unwrap()))
            .unwrap();
        s.grant(&lease("claude", LeaseEpoch::new(3).unwrap()))
            .unwrap();

        let fresh = PilotLeaseStore::new(dir.path());
        assert_eq!(fresh.grants(&mission()).unwrap().len(), 3);
        let cur = fresh.current(&mission()).unwrap().unwrap();
        assert_eq!(cur.epoch, LeaseEpoch::new(3).unwrap());
        assert_eq!(cur.holder_session_id, sid("claude"));
    }

    // If the lines land out of order — two appends racing on one file — the
    // highest epoch still wins, not the last line.
    #[test]
    fn the_highest_epoch_wins_over_file_order() {
        let (_d, s) = store();
        let path = s.grants_path(&mission());
        for l in [
            lease("codex", LeaseEpoch::new(2).unwrap()),
            lease("claude", LeaseEpoch::first()),
        ] {
            append_line(&path, &serde_json::to_string(&l).unwrap()).unwrap();
        }
        assert_eq!(
            s.current(&mission()).unwrap().unwrap().holder_session_id,
            sid("codex")
        );
    }

    #[test]
    fn a_torn_trailing_line_is_skipped_and_grants_nothing() {
        let (_d, s) = store();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.grants_path(&mission()))
            .unwrap();
        write!(f, "{{\"mission_id\":\"task-2026").unwrap();
        drop(f);

        assert_eq!(s.grants(&mission()).unwrap().len(), 1);
        assert_eq!(
            s.current(&mission()).unwrap().unwrap().holder_session_id,
            sid("claude")
        );
    }

    #[test]
    fn the_two_files_live_beside_each_other_under_the_state_root() {
        let s = PilotLeaseStore::new(PathBuf::from("/tmp/state"));
        assert_eq!(
            s.requests_path(&mission()).to_string_lossy(),
            "/tmp/state/pilot-lease/task-20260731-9cf4.requests.jsonl"
        );
        assert_eq!(
            s.grants_path(&mission()).to_string_lossy(),
            "/tmp/state/pilot-lease/task-20260731-9cf4.grants.jsonl"
        );
    }

    // Two missions do not share a lease: authority is per-mission, so a pilot
    // holding one mission is not thereby PRIMARY on another.
    #[test]
    fn missions_do_not_share_authority() {
        let (_d, s) = store();
        let other = MoleculeId::new("task-20260731-0c2d").unwrap();
        s.grant(&lease("claude", LeaseEpoch::first())).unwrap();
        assert!(s.current(&other).unwrap().is_none());
        assert_eq!(
            s.authorize(&other, t0(), &sid("claude"), Some(LeaseEpoch::first()))
                .unwrap(),
            LeaseDecision::Refused(RefusalReason::NoLease)
        );
    }
}
