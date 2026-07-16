// SPDX-License-Identifier: AGPL-3.0-only

//! Livelock detector — Tarjan SCC over the session-wait graph.
//!
//! The detection spec:
//!
//! > Each worker writes `.cosmon/state/presence/<sid>/blocked_on.json`
//! > when parking on external input. Each patrol tick: build the graph
//! > `G = { edge (s_i → s_j) iff s_i.waiting_for ∈ molecules_owned_by(s_j) }`.
//! > Tarjan SCC on G. Non-trivial SCC with all members blocked past
//! > threshold = livelock. **Report, do not auto-resolve** (§8b).
//!
//! This module is the detector half. The patrol `run` path wires it in
//! under `--livelock`, prints a report, and — if a non-trivial SCC is
//! found — invokes `cs nucleate` to raise a `temp:hot` issue molecule
//! tagged `livelock-detected`. It never takes a corrective transport
//! action on behalf of the operator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use cosmon_graph::scc::non_trivial_sccs;
use serde::{Deserialize, Serialize};

/// A parked session — the operator-facing view of a presence record
/// paired with its current `blocked_on.json`.
#[derive(Debug, Clone, Serialize)]
pub struct BlockedSession {
    /// Session id (directory name under `presence/`).
    pub session_id: String,
    /// Molecule the session is currently working on — the node other
    /// sessions reach when they say they wait on it.
    pub current_molecule: Option<String>,
    /// Molecule the session is waiting for (the tail of its arrow in G).
    pub waiting_for: String,
    /// Wall-clock time the session parked — used to apply the staleness
    /// threshold so that sessions whose heartbeat died long ago do not
    /// hold a cycle open forever.
    pub since: DateTime<Utc>,
}

/// One non-trivial SCC = one livelock cycle.
#[derive(Debug, Clone, Serialize)]
pub struct LivelockCycle {
    /// The sessions involved, sorted.
    pub sessions: Vec<String>,
    /// The molecules caught in the cycle, in the same session order.
    pub waiting_on: Vec<String>,
    /// The oldest `since` across the cycle — the dominant blocked-time
    /// the operator should see at-a-glance.
    pub oldest_since: DateTime<Utc>,
}

/// Summary of one livelock sweep.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LivelockReport {
    /// Sessions that published a `blocked_on.json` at probe time.
    pub blocked_sessions: usize,
    /// Non-trivial SCCs found.
    pub cycles: Vec<LivelockCycle>,
    /// Session ids skipped because `since > stale_after`. Kept separate
    /// so the operator can see why a suspected cycle wasn't reported.
    pub stale_sessions: Vec<String>,
}

/// File layout under the state dir — kept as constants so the path
/// rename (future: `presence/`) is a one-line change.
const PRESENCE_SUBDIR: &str = "presence";
const PRESENCE_FILE: &str = "presence.json";
const BLOCKED_FILE: &str = "blocked_on.json";

/// Minimal schema of the presence record this detector reads.
///
/// Duplicated from `cmd::diverge` on purpose: the two commands share
/// the same soft contract with C-PRESENCE-CORE, but their code paths
/// are independent — one lands before the other and neither should
/// block on the promotion of a central type.
#[derive(Debug, Deserialize)]
struct PresenceRecord {
    #[serde(default)]
    current_molecule: Option<String>,
}

/// Minimal schema of `blocked_on.json`.
///
/// Schema (turing §6):
/// ```json
/// { "waiting_for": "mol-id", "since": "2026-04-24T15:02:11Z",
///   "reason": "awaiting user input" }
/// ```
#[derive(Debug, Deserialize)]
struct BlockedOnRecord {
    waiting_for: String,
    since: DateTime<Utc>,
}

/// Scan the presence directory for blocked sessions.
///
/// Idempotent and side-effect-free: reads files, parses, returns.
/// Sessions with missing/malformed `blocked_on.json` are skipped
/// silently — livelock detection is defensive: a corrupt sidecar
/// must never halt the patrol.
pub fn read_blocked_sessions(state_dir: &Path) -> Vec<BlockedSession> {
    let root = state_dir.join(PRESENCE_SUBDIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<BlockedSession> = Vec::new();
    for entry in entries.flatten() {
        let sid = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(blocked) = read_blocked_on(&dir) else {
            continue;
        };
        let current = read_current_molecule(&dir);
        out.push(BlockedSession {
            session_id: sid,
            current_molecule: current,
            waiting_for: blocked.waiting_for,
            since: blocked.since,
        });
    }
    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

fn read_blocked_on(dir: &Path) -> Option<BlockedOnRecord> {
    let p: PathBuf = dir.join(BLOCKED_FILE);
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice::<BlockedOnRecord>(&bytes).ok()
}

fn read_current_molecule(dir: &Path) -> Option<String> {
    let p: PathBuf = dir.join(PRESENCE_FILE);
    let bytes = std::fs::read(&p).ok()?;
    serde_json::from_slice::<PresenceRecord>(&bytes)
        .ok()
        .and_then(|r| r.current_molecule)
}

/// Compute the session-wait graph and its non-trivial SCCs.
///
/// `stale_after` is the age beyond which a `blocked_on.json` is ignored
/// — sessions that died mid-wait must not pin a cycle forever. Pass
/// `None` to include every session regardless of age.
pub fn detect(
    sessions: &[BlockedSession],
    now: DateTime<Utc>,
    stale_after: Option<Duration>,
) -> LivelockReport {
    let mut report = LivelockReport {
        blocked_sessions: sessions.len(),
        ..Default::default()
    };

    // Partition into fresh vs stale.
    let mut fresh: Vec<&BlockedSession> = Vec::new();
    for s in sessions {
        if let Some(limit) = stale_after {
            if now.signed_duration_since(s.since) > limit {
                report.stale_sessions.push(s.session_id.clone());
                continue;
            }
        }
        fresh.push(s);
    }

    // Build: owner(molecule) = session_id. When two sessions claim the
    // same molecule, the first one wins deterministically (sort by sid).
    // That collision is itself a warning sign but not part of the
    // livelock predicate per se — we just want a single ownership arrow.
    let mut owner_of: HashMap<&str, &str> = HashMap::new();
    for s in &fresh {
        if let Some(mol) = s.current_molecule.as_deref() {
            owner_of.entry(mol).or_insert(s.session_id.as_str());
        }
    }

    // Vertices = session ids. Edges: i → j when i waits on a molecule
    // owned by j. Missing owners drop the edge (nobody to wait on).
    let vertices: Vec<String> = fresh.iter().map(|s| s.session_id.clone()).collect();
    let mut edges: Vec<(String, String)> = Vec::new();
    for s in &fresh {
        if let Some(&owner) = owner_of.get(s.waiting_for.as_str()) {
            if owner == s.session_id.as_str() {
                // Self-wait: the session owns the molecule it's waiting for.
                // This IS a one-session livelock — keep the edge so the
                // SCC detector catches it.
                edges.push((s.session_id.clone(), owner.to_owned()));
            } else {
                edges.push((s.session_id.clone(), owner.to_owned()));
            }
        }
    }

    for scc in non_trivial_sccs(&vertices, &edges) {
        // Re-zip sessions to their `waiting_for` molecule in scc order.
        let sessions_set: std::collections::HashSet<&str> =
            scc.iter().map(String::as_str).collect();
        let mut waiting_on: Vec<String> = Vec::new();
        let mut oldest: Option<DateTime<Utc>> = None;
        for sid in &scc {
            if let Some(s) = fresh.iter().find(|s| s.session_id == *sid) {
                waiting_on.push(s.waiting_for.clone());
                oldest = Some(oldest.map_or(s.since, |o| o.min(s.since)));
            } else {
                waiting_on.push(String::new());
            }
        }
        // Keep only cycles where every member's waiter is also in the SCC —
        // a cycle with a "dangling" entry is a near-miss, not a lock.
        let all_in_scc = scc.iter().all(|sid| {
            fresh
                .iter()
                .find(|s| s.session_id == *sid)
                .is_some_and(|s| {
                    owner_of
                        .get(s.waiting_for.as_str())
                        .is_some_and(|o| sessions_set.contains(*o))
                })
        });
        if !all_in_scc {
            continue;
        }
        report.cycles.push(LivelockCycle {
            sessions: scc,
            waiting_on,
            oldest_since: oldest.unwrap_or(now),
        });
    }

    report
}

/// Render a one-line body for the nucleated issue molecule.
///
/// Intended for the molecule's `topic` variable — readable on its own,
/// short enough to fit in `cs observe`. Richer context ends up in the
/// briefing when the operator tackles the issue.
#[must_use]
pub fn render_issue_topic(cycle: &LivelockCycle) -> String {
    let members = cycle.sessions.join(", ");
    let waits = cycle
        .sessions
        .iter()
        .zip(cycle.waiting_on.iter())
        .map(|(s, m)| format!("{s}→{m}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "LIVELOCK detected between sessions [{members}] waiting on [{waits}] since {}",
        cycle.oldest_since.to_rfc3339()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn session(id: &str, owns: Option<&str>, waits: &str, since_sec_ago: i64) -> BlockedSession {
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        BlockedSession {
            session_id: id.to_owned(),
            current_molecule: owns.map(str::to_owned),
            waiting_for: waits.to_owned(),
            since: now - Duration::seconds(since_sec_ago),
        }
    }

    #[test]
    fn empty_returns_no_cycles() {
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&[], now, None);
        assert_eq!(report.blocked_sessions, 0);
        assert!(report.cycles.is_empty());
    }

    #[test]
    fn two_session_livelock_detected() {
        // Classic turing scenario: A owns X waiting on Y; B owns Y waiting on X.
        let sessions = vec![
            session("A", Some("X"), "Y", 60),
            session("B", Some("Y"), "X", 60),
        ];
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&sessions, now, None);
        assert_eq!(report.cycles.len(), 1);
        let cycle = &report.cycles[0];
        assert_eq!(cycle.sessions, vec!["A".to_owned(), "B".to_owned()]);
    }

    #[test]
    fn linear_wait_chain_is_not_a_livelock() {
        // A waits on X (owned by B), B waits on Y (owned by C), C waits on nothing.
        let sessions = vec![
            session("A", Some("W"), "X", 60),
            session("B", Some("X"), "Y", 60),
            session("C", Some("Y"), "Z", 60),
        ];
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&sessions, now, None);
        assert!(report.cycles.is_empty());
    }

    #[test]
    fn stale_sessions_are_filtered() {
        let sessions = vec![
            session("A", Some("X"), "Y", 3600 * 2), // 2h old — stale
            session("B", Some("Y"), "X", 60),
        ];
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&sessions, now, Some(Duration::hours(1)));
        assert!(report.cycles.is_empty(), "A is filtered, B is alone");
        assert_eq!(report.stale_sessions, vec!["A".to_owned()]);
    }

    #[test]
    fn three_session_cycle_detected() {
        let sessions = vec![
            session("A", Some("X"), "Y", 60),
            session("B", Some("Y"), "Z", 60),
            session("C", Some("Z"), "X", 60),
        ];
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&sessions, now, None);
        assert_eq!(report.cycles.len(), 1);
        assert_eq!(report.cycles[0].sessions.len(), 3);
    }

    #[test]
    fn self_wait_single_session_is_livelock() {
        // Pathological: a session owning X that waits on X.
        let sessions = vec![session("A", Some("X"), "X", 60)];
        let now = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let report = detect(&sessions, now, None);
        assert_eq!(report.cycles.len(), 1);
    }

    #[test]
    fn render_topic_is_readable() {
        let cycle = LivelockCycle {
            sessions: vec!["A".into(), "B".into()],
            waiting_on: vec!["Y".into(), "X".into()],
            oldest_since: Utc.with_ymd_and_hms(2026, 4, 24, 11, 59, 0).unwrap(),
        };
        let s = render_issue_topic(&cycle);
        assert!(s.contains("A, B"));
        assert!(s.contains("A→Y"));
        assert!(s.contains("B→X"));
    }

    #[test]
    fn reads_blocked_sessions_from_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pres = tmp.path().join(PRESENCE_SUBDIR).join("sid-1");
        std::fs::create_dir_all(&pres).unwrap();
        std::fs::write(
            pres.join(PRESENCE_FILE),
            br#"{"cwd":"/tmp","current_molecule":"mol-x"}"#,
        )
        .unwrap();
        std::fs::write(
            pres.join(BLOCKED_FILE),
            br#"{"waiting_for":"mol-y","since":"2026-04-24T12:00:00Z"}"#,
        )
        .unwrap();
        let sessions = read_blocked_sessions(tmp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sid-1");
        assert_eq!(sessions[0].waiting_for, "mol-y");
        assert_eq!(sessions[0].current_molecule.as_deref(), Some("mol-x"));
    }

    #[test]
    fn corrupt_blocked_on_json_is_skipped_not_fatal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pres = tmp.path().join(PRESENCE_SUBDIR).join("sid-bad");
        std::fs::create_dir_all(&pres).unwrap();
        std::fs::write(pres.join(BLOCKED_FILE), b"not valid json{{").unwrap();
        let sessions = read_blocked_sessions(tmp.path());
        assert!(sessions.is_empty());
    }
}
