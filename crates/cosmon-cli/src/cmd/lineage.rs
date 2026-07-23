// SPDX-License-Identifier: AGPL-3.0-only

//! Scheduler-derived merge-commit lineage trailers (delib-20260720-cff4, Phase 1).
//!
//! The dependency DAG is the source of truth; git history is a *projection*
//! of it (the panel's unanimous framing). This module makes that projection
//! **legible** without changing any merge topology: it turns the scheduler's
//! ledgered `blocked_by` edges into git trailers stamped on the completion
//! merge that `cs done` already creates, and it parses them back when a
//! read-only view (`cs mission graph`) reconstructs the mission tree.
//!
//! # Why the values must come from the ledger, never from the worker
//!
//! buterin's hard constraint (synthesis §"Surprising insights"): if the
//! lineage annotations were read from anything a worker can write — the
//! commit body it authored, a file in its worktree — the legibility layer
//! would become a *forgery channel*, a place to claim a dependency edge that
//! the scheduler never ledgered. Every value here is therefore derived from
//! [`MoleculeData`](cosmon_state::MoleculeData) loaded from the canonical
//! [`StateStore`] at `cs done` time: the `blocked_by()` edges recorded at
//! nucleation, which no worker
//! step mutates. The trailer is a *witness* of the ledger, not a second
//! authored copy of it.
//!
//! # The trailers
//!
//! On a **completion merge** (`cs done` folding `feat/<mol>` into main):
//!
//! ```text
//! Mol-Id: <this molecule's id>
//! Mission-Id: <the DAG root this molecule belongs to>
//! Depends-On: <parent id>      (one line per direct blocked_by parent)
//! ```
//!
//! On a **base-sync merge** (`cs sync` folding main into `feat/<mol>`):
//!
//! ```text
//! Base-Sync: <base>..<branch>
//! ```
//!
//! The `Base-Sync` trailer replaces the load-bearing *direction heuristic*
//! (torvalds, synthesis §"Surprising insights"): today the only thing
//! distinguishing a base-sync from a completion merge is the merge direction
//! (`main→feat` vs `feat→main`). An explicit, durable marker is unambiguous
//! and hardens the provenance gate's base-sync recognition without relaxing
//! its structural safety check.

use std::collections::HashSet;

use cosmon_core::id::MoleculeId;
use cosmon_state::StateStore;

/// Trailer key carrying the completed molecule's own id.
pub const MOL_ID_KEY: &str = "Mol-Id";
/// Trailer key carrying the DAG root (mission) the molecule belongs to.
pub const MISSION_ID_KEY: &str = "Mission-Id";
/// Trailer key carrying a single direct `blocked_by` parent id. Repeated
/// once per parent, matching git's trailer convention for multi-valued keys.
pub const DEPENDS_ON_KEY: &str = "Depends-On";
/// Trailer key marking a base-sync merge and naming its `<base>..<branch>` span.
pub const BASE_SYNC_KEY: &str = "Base-Sync";

/// Resolve the *mission root* of `mol_id`: the DAG root reached by walking
/// `blocked_by` edges upward until a molecule with no upstream blockers is
/// found.
///
/// A well-formed mission is a rooted DAG, so a single root is the common
/// case. When fan-in exposes more than one reachable root (a molecule
/// transitively blocked by two independent origins), the **lexicographically
/// smallest** root id is chosen so the mission id is deterministic and stable
/// across runs — the same molecule always reports the same mission. A
/// molecule with no `blocked_by` edges is its own mission root.
///
/// Missing or unreadable parents are skipped rather than fatal: the walk is a
/// best-effort projection for a human-facing view, never a gate. Cycles
/// (which a valid DAG never contains, but a corrupt ledger might) are made
/// safe by a visited set.
#[must_use]
pub fn mission_root(store: &dyn StateStore, mol_id: &MoleculeId) -> MoleculeId {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    seen.insert(mol_id.clone());
    let mut frontier: Vec<MoleculeId> = vec![mol_id.clone()];
    let mut roots: Vec<MoleculeId> = Vec::new();

    while let Some(id) = frontier.pop() {
        match store.load_molecule(&id) {
            Ok(mol) => {
                let parents: Vec<MoleculeId> = mol.blocked_by().into_iter().cloned().collect();
                if parents.is_empty() {
                    // No upstream blockers — this node is a mission root.
                    roots.push(id);
                } else {
                    for p in parents {
                        if seen.insert(p.clone()) {
                            frontier.push(p);
                        }
                    }
                }
            }
            // A dangling parent reference terminates that arm of the walk;
            // treat the last reachable id as a root candidate so a broken
            // edge cannot erase the mission entirely.
            Err(_) => roots.push(id),
        }
    }

    roots.into_iter().min().unwrap_or_else(|| mol_id.clone())
}

/// Build the completion-merge lineage trailers for `mol_id`, derived entirely
/// from the ledger (never from worker-writable state).
///
/// Returns, in order: one `Mol-Id`, one `Mission-Id`, then one `Depends-On`
/// per direct `blocked_by` parent (in the molecule's ledgered order). The
/// lines are ready to be joined into a single trailer paragraph.
#[must_use]
pub fn completion_trailers(store: &dyn StateStore, mol_id: &MoleculeId) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("{MOL_ID_KEY}: {}", mol_id.as_str()));
    let mission = mission_root(store, mol_id);
    out.push(format!("{MISSION_ID_KEY}: {}", mission.as_str()));
    if let Ok(mol) = store.load_molecule(mol_id) {
        for parent in mol.blocked_by() {
            out.push(format!("{DEPENDS_ON_KEY}: {}", parent.as_str()));
        }
    }
    out
}

/// Build the base-sync trailer naming the `<base>..<branch>` span this merge
/// synchronised. A single line; ready to join into a trailer paragraph.
#[must_use]
pub fn base_sync_trailer(base: &str, branch: &str) -> String {
    format!("{BASE_SYNC_KEY}: {base}..{branch}")
}

/// Parse the trailer lines (`Key: value`) out of a git commit message body.
///
/// Deliberately lenient — it scans every line for a `Key: value` shape rather
/// than isolating the strict final trailer block, because the completion
/// merge stamps its trailers as a dedicated paragraph and callers only look
/// up the lineage keys. Returns `(key, value)` pairs in file order.
#[must_use]
pub fn parse_trailers(message: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in message.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(": ") {
            // A trailer key is a single token with no spaces (RFC-822-ish).
            if !key.is_empty() && !key.contains(char::is_whitespace) {
                out.push((key.to_owned(), value.trim().to_owned()));
            }
        }
    }
    out
}

/// Extract the single value of the first trailer whose key equals `key`.
#[must_use]
pub fn trailer_value(message: &str, key: &str) -> Option<String> {
    parse_trailers(message)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// Extract every value of the (possibly repeated) trailer `key`.
///
/// Used by read-only consumers that reconstruct multi-valued edges (e.g. the
/// `Depends-On` fan-in) directly from a merge commit rather than the ledger.
#[must_use]
#[cfg_attr(not(test), allow(dead_code))]
pub fn trailer_values(message: &str, key: &str) -> Vec<String> {
    parse_trailers(message)
        .into_iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    /// Build A -blocks-> B -blocks-> C, persist, return ids.
    fn chain(store: &FileStore) -> (MoleculeId, MoleculeId, MoleculeId) {
        let a = MoleculeId::new("task-20260720-aaaa").unwrap();
        let b = MoleculeId::new("task-20260720-bbbb").unwrap();
        let c = MoleculeId::new("task-20260720-cccc").unwrap();

        let mut ma = sample(a.as_str());
        ma.typed_links
            .push(MoleculeLink::Blocks { target: b.clone() });

        let mut mb = sample(b.as_str());
        mb.typed_links
            .push(MoleculeLink::BlockedBy { source: a.clone() });
        mb.typed_links
            .push(MoleculeLink::Blocks { target: c.clone() });

        let mut mc = sample(c.as_str());
        mc.typed_links
            .push(MoleculeLink::BlockedBy { source: b.clone() });

        store.save_molecule(&a, &ma).unwrap();
        store.save_molecule(&b, &mb).unwrap();
        store.save_molecule(&c, &mc).unwrap();
        (a, b, c)
    }

    #[test]
    fn mission_root_walks_chain_to_top() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let (a, _b, c) = chain(&store);
        // C is blocked_by B blocked_by A → mission root is A.
        assert_eq!(mission_root(&store, &c), a);
    }

    #[test]
    fn mission_root_of_a_root_is_itself() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let (a, _b, _c) = chain(&store);
        assert_eq!(mission_root(&store, &a), a);
    }

    #[test]
    fn completion_trailers_carry_mol_mission_and_parents() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let (a, _b, c) = chain(&store);
        let trailers = completion_trailers(&store, &c);
        assert_eq!(trailers[0], "Mol-Id: task-20260720-cccc");
        assert_eq!(trailers[1], format!("Mission-Id: {}", a.as_str()));
        assert_eq!(trailers[2], "Depends-On: task-20260720-bbbb");
        assert_eq!(trailers.len(), 3);
    }

    #[test]
    fn completion_trailers_root_has_no_depends_on() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let (a, _b, _c) = chain(&store);
        let trailers = completion_trailers(&store, &a);
        // Mol-Id + Mission-Id, no Depends-On.
        assert_eq!(trailers.len(), 2);
        assert!(trailers.iter().all(|t| !t.starts_with("Depends-On")));
    }

    #[test]
    fn mission_root_multiple_roots_picks_smallest_deterministically() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        // C blocked_by both A (zzzz) and B (aaaa), both roots.
        let a = MoleculeId::new("task-20260720-zzzz").unwrap();
        let b = MoleculeId::new("task-20260720-0000").unwrap();
        let c = MoleculeId::new("task-20260720-cccc").unwrap();
        let ma = sample(a.as_str());
        let mb = sample(b.as_str());
        let mut mc = sample(c.as_str());
        mc.typed_links
            .push(MoleculeLink::BlockedBy { source: a.clone() });
        mc.typed_links
            .push(MoleculeLink::BlockedBy { source: b.clone() });
        store.save_molecule(&a, &ma).unwrap();
        store.save_molecule(&b, &mb).unwrap();
        store.save_molecule(&c, &mc).unwrap();
        // "task-20260720-0000" < "task-20260720-zzzz" lexicographically.
        assert_eq!(mission_root(&store, &c), b);
    }

    #[test]
    fn base_sync_trailer_names_the_span() {
        assert_eq!(
            base_sync_trailer("main", "feat/task-20260720-cccc"),
            "Base-Sync: main..feat/task-20260720-cccc"
        );
    }

    #[test]
    fn parse_and_lookup_trailers_round_trip() {
        let msg = "Merge branch 'feat/task-20260720-cccc'\n\n\
                   Mol-Id: task-20260720-cccc\n\
                   Mission-Id: task-20260720-aaaa\n\
                   Depends-On: task-20260720-bbbb\n\
                   Depends-On: task-20260720-dddd\n";
        assert_eq!(
            trailer_value(msg, MOL_ID_KEY).as_deref(),
            Some("task-20260720-cccc")
        );
        assert_eq!(
            trailer_value(msg, MISSION_ID_KEY).as_deref(),
            Some("task-20260720-aaaa")
        );
        assert_eq!(
            trailer_values(msg, DEPENDS_ON_KEY),
            vec![
                "task-20260720-bbbb".to_owned(),
                "task-20260720-dddd".to_owned()
            ]
        );
        assert_eq!(trailer_value(msg, BASE_SYNC_KEY), None);
    }

    #[test]
    fn parse_trailers_ignores_prose_colons() {
        // A body line with a colon but a multi-word key is not a trailer.
        let msg = "subject\n\nsee also: nothing here\nMol-Id: task-20260720-cccc\n";
        assert_eq!(
            trailer_value(msg, MOL_ID_KEY).as_deref(),
            Some("task-20260720-cccc")
        );
        // "see also" has a space in the key → not parsed as a trailer key.
        assert!(parse_trailers(msg).iter().all(|(k, _)| k != "see also"));
    }
}
