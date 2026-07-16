// SPDX-License-Identifier: AGPL-3.0-only

//! Spec-conformance audit types — the Rust side of the `π` refinement map.
//!
//! This module sits between [`crate::event_v2`] (what the system actually
//! recorded) and [`crate::spec`] (what the TLA+ spec sanctioned). It is the
//! refinement mapping `π` made concrete — a small, pure, serde-derivable
//! set of types that the `cs spec-audit` command replays to catch the
//! **disabled-merge bug** (a branch merged onto `main` while the molecule's
//! status is still `Pending` — i.e. a `git merge` that bypassed
//! `cs done`), the **stale-pending cascade** (molecules left pending that a
//! greedy run resurrects), and any other drift where the observed ledger
//! contradicts the sanctioned transition system.
//!
//! Any new [`EventV2`] variant that carries a state-machine action should be
//! mapped onto the action alphabet (see [`Action::from_event`]) in the same
//! commit that adds it; otherwise the projection silently drops it and the
//! audit under-reports drift.
//!
//! ## What this module is **not**
//!
//! * It is not a runtime trace-check daemon — that approach was deliberately
//!   rejected as too heavyweight for the value it would add.
//! * It does not mutate ledgers, re-index files, or auto-remediate. Callers
//!   receive a structured [`AuditReport`] and decide what to do.
//! * It does not duplicate the proptest spec-conformance harness in
//!   `cosmon-core/tests/spec_conformance.rs`; this is batch replay of real
//!   ledgers, not property-based exploration.

use serde::{Deserialize, Serialize};

use crate::event_v2::{Envelope, EventV2, Seq};
use crate::id::MoleculeId;
use crate::spec::Action;

// ---------------------------------------------------------------------------
// Action ← EventV2 projection (π)
// ---------------------------------------------------------------------------

impl Action {
    /// Project an [`EventV2`] onto the TLA+ action alphabet.
    ///
    /// Returns `None` for events that are not state-machine actions (worker
    /// telemetry, energy ticks, heartbeats, …). The audit loop ignores
    /// non-action events — they do not contribute to the spec transition
    /// relation.
    ///
    /// # Correspondence
    ///
    /// Each arm below maps one [`EventV2`] variant to the TLA+ action that
    /// would fire under the spec. Adding an [`EventV2`] variant that is
    /// state-machine-relevant **must** add a match arm here, otherwise the
    /// projection silently drops it (and the audit under-reports drift).
    #[must_use]
    pub fn from_event(event: &EventV2) -> Option<Action> {
        match event {
            EventV2::MoleculeNucleated { .. } => Some(Action::Nucleate),
            EventV2::MoleculeStatusChanged { to, .. } => match to.as_str() {
                "running" => Some(Action::Tackle),
                "frozen" => Some(Action::Freeze),
                // `thawed` → back to Running; `pending` after thaw is rare
                // but in both cases Thaw is the matching spec action.
                "thawed" | "pending" => Some(Action::Thaw),
                // Completed / Collapsed also emit status_changed, but the
                // explicit terminal events are the authoritative signals.
                _ => None,
            },
            EventV2::MoleculeStepCompleted { .. } => Some(Action::Evolve),
            EventV2::MoleculeCompleted { .. } => Some(Action::Complete),
            EventV2::MoleculeCollapsed { .. } => Some(Action::Collapse),
            EventV2::MergeCompleted { .. } => Some(Action::Done),
            EventV2::WorkerKilled { .. } => Some(Action::Purge),
            EventV2::WorkerExited { .. } => Some(Action::ProcessCrash),
            // Every other variant is telemetry / receipt / advisory — the
            // state machine is not affected. Listing them exhaustively keeps
            // this match honest when new variants are added.
            EventV2::MoleculeStuck { .. }
            | EventV2::DecaySpliced { .. }
            | EventV2::MergeDispatched { .. }
            | EventV2::WorkerSpawned { .. }
            | EventV2::WorkerHeartbeat { .. }
            | EventV2::EnergyTick { .. }
            | EventV2::Expired { .. }
            | EventV2::GateStarted { .. }
            | EventV2::GateCompleted { .. }
            | EventV2::GateFailed { .. }
            | EventV2::NativeStarted { .. }
            | EventV2::NativeCompleted { .. }
            | EventV2::NativeFailed { .. }
            | EventV2::RuntimeGuardOverride { .. }
            | EventV2::PromptSealed { .. }
            | EventV2::BriefingSealed { .. }
            | EventV2::BootstrapSealed { .. }
            | EventV2::SealAttested { .. }
            | EventV2::SealBypassed { .. }
            | EventV2::Resurrected { .. }
            | EventV2::Harvested { .. }
            | EventV2::WorkerSilenceDetected { .. }
            // Blocking-dialogue detection is a patrol observation of pane
            // state (ADR-137 §2) — advisory telemetry, never a lifecycle
            // transition. The molecule stays `Running` whether the dialog is
            // auto-confirmed or the operator is paged.
            | EventV2::BlockingDialogueDetected { .. }
            // ADR-123: a worker blocked on an operator is an advisory
            // annotation on a still-`Running` molecule, not a lifecycle
            // transition — the molecule does not change status. Telemetry,
            // not a spec action.
            | EventV2::WorkerBlockedOnOperator { .. }
            | EventV2::QueryStepEvaluated { .. }
            | EventV2::ExternalChannelTimeout { .. }
            | EventV2::InvocationCompleted { .. }
            | EventV2::FleetTyped { .. }
            | EventV2::OperatorPresent { .. }
            | EventV2::OperatorAbsent { .. }
            | EventV2::OperatorSpark { .. }
            | EventV2::OperatorVerdict { .. }
            | EventV2::OperatorRefused { .. }
            | EventV2::OperatorSilent { .. }
            | EventV2::OperatorSigned { .. }
            | EventV2::RuntimeReadDecideWrite { .. }
            | EventV2::RuntimeShelledOut { .. }
            | EventV2::RuntimeMergeDispatched { .. }
            | EventV2::RuntimeWorktreeClaimed { .. }
            // ADR-097 Worker-Spawn Port events are pure IFBDD instrumentation;
            // they observe the adapter's lifecycle without driving the spec
            // transition system. Adding spec-relevant adapter actions later
            // means promoting them out of this telemetry block.
            | EventV2::WorkerSpawnAttempted { .. }
            | EventV2::WorkerSpawnFailed { .. }
            | EventV2::WorkerSpawnRolledBack { .. }
            | EventV2::AdapterLivenessProbed { .. }
            | EventV2::AdapterPaneSignatureChecked { .. }
            | EventV2::AdapterBriefingConsumed { .. }
            | EventV2::AdapterHandleReconciled { .. }
            | EventV2::AdapterSelected { .. }
            // Model-selection attribution (delib-20260704-b476 / C2) is the
            // model sibling of AdapterSelected — a forensic receipt of which
            // model was pinned and where the choice came from. It observes
            // the spawn without driving a spec transition.
            | EventV2::ModelSelected { .. }
            // Model-budget ceiling receipt (delib-20260704-b476 / C4) is a
            // forensic record that the fail-closed strong-dispatch ceiling
            // fired (downgraded or aborted a strong pin). It observes the
            // spawn-gate decision without driving a spec transition.
            | EventV2::ModelCeilingHit { .. }
            // Autonomy-guard receipts (task-20260530-d8bc) are forensic-only:
            // RemoteEgressOptIn records that egress was granted before spawn,
            // LocalExecReceipt records positive per-turn local-exec evidence.
            // Neither drives a spec transition. LocalFallback (Q5b,
            // task-20260530-c089) is the same: a forensic record that a local
            // hard-failure was consciously escalated to a remote oracle —
            // routing provenance, not a spec action.
            | EventV2::RemoteEgressOptIn { .. }
            // EgressUnenforceable (C1-F3, task-20260712-8d2d) records that a
            // `deny-external` dispatch degraded to advisory on a host that
            // cannot build the netns jail — forensic routing provenance the
            // cutover gate reads, not a spec transition.
            | EventV2::EgressUnenforceable { .. }
            | EventV2::LocalFallback { .. }
            | EventV2::LocalExecReceipt { .. }
            // ADR-100 SF-1..SF-5 are forensic-only silent-failure receipts
            // emitted by Direct-API Adapters; they do not drive the spec
            // transition system either. SF-6 is the sibling cosmon-lab
            // supervision-setup-failure receipt emitted by `cs tackle`; same
            // forensic-only contract — the event records that supervision is
            // missing without claiming or denying any spec transition. SF-7
            // (delib-20260518-5178 §C9) is the codex-Adapter pre-spawn
            // version-pin mismatch receipt; same forensic-only contract.
            | EventV2::SF1HttpTransport { .. }
            | EventV2::SF2ProviderRateLimit { .. }
            | EventV2::SF3SchemaDecodeFailure { .. }
            | EventV2::SF4ToolCallExecutionFailure { .. }
            | EventV2::SF5ContextOverflow { .. }
            | EventV2::SF6SupervisionSetupFailed { .. }
            | EventV2::SF7BinaryVersionMismatch { .. }
            // ADR-105 I9' machinery: ChronicleAdded / AdrInscribed are
            // documentation receipts. They do not drive the spec
            // transition system; they exist to carry the
            // `federation_provenance` field that `cs verify --federation`
            // lints. Adding them to the action alphabet would conflate
            // *the rule was written down* with *the rule fired*.
            | EventV2::ChronicleAdded { .. }
            | EventV2::AdrInscribed { .. }
            // Config-honoring dispatch (delib-20260531-c761): a forensic
            // refusal receipt, not a spec action. It records that the
            // runtime *declined* to dispatch on a stale launch snapshot;
            // the spec transition system never sees a fired action because
            // none fired — that absence is the whole point.
            | EventV2::ConfigDriftDetected { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Drift — the audit's output unit
// ---------------------------------------------------------------------------

/// A single spec-conformance drift detected during replay.
///
/// Drifts are ordered in the report by `seq` (the global envelope sequence
/// at which they were detected); the earliest drift is the actionable one,
/// downstream drifts are often cascades of the first failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Drift {
    /// A sanctioned action fired while the spec said it was disabled.
    ///
    /// Example: `MergeCompleted` observed while the molecule's spec status
    /// was `Pending` — a branch merged onto `main` without ever having
    /// been tackled or completed.
    DisabledActionFired {
        /// Envelope sequence at which the offending event lived.
        seq: Seq,
        /// The molecule the event is about, when the event carries one.
        molecule_id: Option<MoleculeId>,
        /// The action the projection mapped the event to.
        action: Action,
        /// One-line human-readable note (why the spec disabled this action).
        note: String,
    },
    /// `branch_merged` was observed `TRUE` out-of-band while the spec
    /// status was still `Pending`.
    ///
    /// This is the out-of-band merge witness — the TLA+ `BypassMerge`
    /// action — surfaced by a git-topology probe rather than an in-band
    /// event: the branch landed on `main` while the ledger still shows the
    /// molecule as never-completed.
    BypassMerge {
        /// Envelope sequence at which the probe fired (the audit attaches
        /// the probe to the envelope that prompted the check).
        seq: Seq,
        /// The molecule whose branch was merged without a sanctioned Done.
        molecule_id: MoleculeId,
    },
    /// The projection did not know how to map an event. Emitted only when
    /// the caller has opted into strict reporting; the default audit
    /// silently ignores telemetry events.
    UnmappedEvent {
        /// Envelope sequence at which the event lived.
        seq: Seq,
        /// The molecule the event is about, when the event carries one.
        molecule_id: Option<MoleculeId>,
        /// Short variant tag (e.g. `"molecule_status_changed"`).
        variant: String,
    },
    /// A spec-specific invariant was violated.
    ///
    /// Catch-all used by spec auditors whose drift shape does not fit
    /// the CosmonRun-shaped variants above — notably the noogram
    /// auditors (`MycelialGate`, `AttestorGraph`, `WitnessFreshness`).
    /// The `spec` field names the auditor; `invariant` is a stable
    /// string tag readers can switch on. Adding a new noogram auditor
    /// only extends the `(spec, invariant)` value space — no new
    /// top-level Drift variant is needed.
    SpecInvariantViolation {
        /// Envelope sequence at which the offending row lived.
        seq: Seq,
        /// Spec auditor that emitted this drift (e.g. `"mycelial-gate"`).
        spec: String,
        /// Stable, machine-readable invariant tag
        /// (e.g. `"insufficient_witnesses"`).
        invariant: String,
        /// Optional subject the drift is about (an attestor id, an
        /// absorption id, …). Auditors keep this short and grep-able.
        subject: Option<String>,
        /// One-line human-readable note for the operator.
        note: String,
    },
}

impl Drift {
    /// Envelope sequence at which the drift was detected.
    #[must_use]
    pub fn seq(&self) -> Seq {
        match self {
            Self::DisabledActionFired { seq, .. }
            | Self::BypassMerge { seq, .. }
            | Self::UnmappedEvent { seq, .. }
            | Self::SpecInvariantViolation { seq, .. } => *seq,
        }
    }

    /// The molecule associated with the drift, when known.
    ///
    /// `SpecInvariantViolation` drifts are not molecule-scoped (they
    /// belong to a noogram spec, not a cosmon molecule), so this
    /// returns `None` for them.
    #[must_use]
    pub fn molecule_id(&self) -> Option<&MoleculeId> {
        match self {
            Self::DisabledActionFired { molecule_id, .. }
            | Self::UnmappedEvent { molecule_id, .. } => molecule_id.as_ref(),
            Self::BypassMerge { molecule_id, .. } => Some(molecule_id),
            Self::SpecInvariantViolation { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// AuditReport — the full replay outcome
// ---------------------------------------------------------------------------

/// The output of a spec audit pass over one fleet's `events.jsonl`.
///
/// `drifts` is sorted by envelope sequence; `events_replayed` and
/// `molecules_seen` give the operator a sanity-check on coverage (did the
/// audit actually walk the file?). The report is serde-derivable so the
/// `--json` CLI mode prints it verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditReport {
    /// Every drift detected, in envelope-sequence order.
    pub drifts: Vec<Drift>,
    /// Number of envelopes the auditor processed.
    pub events_replayed: u64,
    /// Number of distinct molecules seen in the trace.
    pub molecules_seen: u64,
}

impl AuditReport {
    /// `true` iff no drifts were detected.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.drifts.is_empty()
    }
}

// ---------------------------------------------------------------------------
// BranchMergedProbe — the out-of-band observer (ledger audit only)
// ---------------------------------------------------------------------------

/// Result of probing whether a molecule's branch has been merged.
///
/// The audit loop uses this to detect the bypassed-merge bug: a
/// `branch_merged = TRUE` observation while the spec `status = Pending`
/// is the fingerprint of a merge that skipped `cs done`. Callers supply
/// the probe; the audit module does not do I/O.
pub trait BranchMergedProbe {
    /// `true` iff the branch for `molecule` has been merged (e.g.
    /// `git merge-base --is-ancestor feat/<mol> origin/main`).
    /// `false` iff the branch has not been merged. `None` iff the probe
    /// could not decide (no branch, git unavailable, …).
    fn is_branch_merged(&self, molecule: &MoleculeId) -> Option<bool>;
}

/// A probe that always returns `None` — the safe default when no git
/// topology check is available. The audit still replays the ledger and
/// still catches disabled-action-fired drifts; only the out-of-band
/// bypassed-merge check is silenced.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProbe;

impl BranchMergedProbe for NullProbe {
    fn is_branch_merged(&self, _molecule: &MoleculeId) -> Option<bool> {
        None
    }
}

// ---------------------------------------------------------------------------
// Pure replay driver
// ---------------------------------------------------------------------------

/// Per-molecule spec state — the audit replay walks one state per molecule.
///
/// Exposed as a distinct struct (rather than leaking the `HashMap` shape into
/// [`audit_trace`]) because future callers may want per-molecule introspection
/// (e.g. `cs spec-audit --molecule <id>` replaying a single life-cycle).
type MolMap = std::collections::HashMap<MoleculeId, crate::spec::SpecState>;

/// Replay a sequence of envelopes through [`crate::spec::SpecState`] and
/// return an [`AuditReport`].
///
/// Each molecule gets its own fresh [`SpecState::init_single`][init] at
/// first sight; the audit is multi-molecule by structure (one state per
/// molecule) while [`crate::spec::SpecState`] remains the single-molecule
/// projection the Chantier 1 harness locks down.
///
/// The out-of-band probe is consulted **once per molecule after the full
/// replay**. The bypassed-merge fingerprint is a disagreement between the
/// ledger's final sanctioned state (`spec.branch_merged()`) and the external
/// git-topology truth (`probe.is_branch_merged(mol) == Some(true)`). If
/// the ledger thinks the branch is unmerged but git says it is merged,
/// the merge happened outside `cs done` — the bypass witness.
///
/// Pass [`NullProbe`] if no git topology check is available; the audit
/// still reports disabled-action-fired drifts in that mode.
///
/// [init]: crate::spec::SpecState::init_single
pub fn audit_trace<P: BranchMergedProbe>(envelopes: &[Envelope], probe: &P) -> AuditReport {
    let mut states: MolMap = MolMap::new();
    let mut drifts = Vec::new();
    let mut seen: std::collections::HashSet<MoleculeId> = std::collections::HashSet::new();
    let mut last_seq_for_mol: std::collections::HashMap<MoleculeId, Seq> =
        std::collections::HashMap::new();

    for env in envelopes {
        let mol = env.event.molecule_id().cloned();
        if let Some(ref m) = mol {
            seen.insert(m.clone());
            last_seq_for_mol.insert(m.clone(), env.seq);
        }

        // Non-action events are telemetry — record the molecule for probe
        // coverage but do not step the spec.
        let Some(action) = Action::from_event(&env.event) else {
            continue;
        };

        // Audit pass is per-molecule; a molecule-less action (none exist
        // in the current alphabet, but guard anyway) is an immediate drift.
        let Some(mol_id) = mol else {
            drifts.push(Drift::DisabledActionFired {
                seq: env.seq,
                molecule_id: None,
                action,
                note: "action event without molecule_id".to_owned(),
            });
            continue;
        };

        let state = states
            .entry(mol_id.clone())
            .or_insert_with(crate::spec::SpecState::init_single);

        if !state.enabled(action) {
            drifts.push(Drift::DisabledActionFired {
                seq: env.seq,
                molecule_id: Some(mol_id),
                action,
                note: format!(
                    "spec.enabled({action:?}) was false at status={:?}",
                    state.status()
                ),
            });
            // Continue without stepping: matches TLA+ semantics where a
            // disabled disjunct cannot fire. The audit still records
            // downstream drifts as they arise.
            continue;
        }

        state.step(action);
    }

    // Out-of-band pass: for every molecule the ledger knows about,
    // compare the spec's `branch_merged` flag against the external
    // probe. A `probe = true` while `spec.branch_merged() = false` is
    // the c1cb fingerprint — the branch landed on main without a
    // sanctioned `cs done`.
    let mut bypass_mols: Vec<_> = seen
        .iter()
        .filter(|mol_id| {
            let state = states
                .entry((*mol_id).clone())
                .or_insert_with(crate::spec::SpecState::init_single);
            !state.branch_merged() && probe.is_branch_merged(mol_id) == Some(true)
        })
        .cloned()
        .collect();
    bypass_mols.sort();
    for mol_id in bypass_mols {
        let seq = last_seq_for_mol.get(&mol_id).copied().unwrap_or(Seq(0));
        drifts.push(Drift::BypassMerge {
            seq,
            molecule_id: mol_id,
        });
    }

    drifts.sort_by_key(Drift::seq);

    AuditReport {
        drifts,
        events_replayed: envelopes.len() as u64,
        molecules_seen: seen.len() as u64,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_v2::{EventV2, MergeResult};

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn env(seq: u64, event: EventV2) -> Envelope {
        Envelope::new(Seq(seq), None, event)
    }

    struct ConstProbe(std::collections::HashMap<MoleculeId, bool>);

    impl BranchMergedProbe for ConstProbe {
        fn is_branch_merged(&self, m: &MoleculeId) -> Option<bool> {
            self.0.get(m).copied()
        }
    }

    #[test]
    fn nucleated_projects_to_nucleate_action() {
        let e = EventV2::MoleculeNucleated {
            molecule_id: mid("cs-20260419-aaaa"),
            formula_id: "task-work".into(),
            parent_id: None,
            blocks: Vec::new(),
        };
        assert_eq!(Action::from_event(&e), Some(Action::Nucleate));
    }

    #[test]
    fn heartbeat_has_no_projection() {
        let e = EventV2::EnergyTick {
            worker_id: crate::id::WorkerId::new("quartz").unwrap(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        };
        assert_eq!(Action::from_event(&e), None);
    }

    #[test]
    fn clean_happy_path_emits_no_drifts() {
        let m = mid("cs-20260419-aaaa");
        let log = vec![
            env(
                0,
                EventV2::MoleculeNucleated {
                    molecule_id: m.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
            ),
            env(
                1,
                EventV2::MoleculeStatusChanged {
                    molecule_id: m.clone(),
                    from: "pending".into(),
                    to: "running".into(),
                },
            ),
            env(
                2,
                EventV2::MoleculeStepCompleted {
                    molecule_id: m.clone(),
                    step: 0,
                    total: 2,
                    duration_ms: Some(100),
                    step_hash: None,
                },
            ),
            env(
                3,
                EventV2::MoleculeCompleted {
                    molecule_id: m.clone(),
                    duration_ms: Some(1000),
                    reason: "ok".into(),
                },
            ),
            env(
                4,
                EventV2::MergeCompleted {
                    molecule: m.clone(),
                    branch: "feat/task-x".into(),
                    result: MergeResult::Ok,
                    federation_provenance: None,
                },
            ),
        ];
        let report = audit_trace(&log, &NullProbe);
        assert!(report.is_clean(), "drifts={:?}", report.drifts);
        assert_eq!(report.events_replayed, 5);
        assert_eq!(report.molecules_seen, 1);
    }

    #[test]
    fn c1cb_bypass_merge_witness_is_flagged() {
        // Scenario: nucleated but never tackled; git topology says the
        // branch is merged. That is the c1cb morning-merge fingerprint.
        let m = mid("cs-20260419-bbbb");
        let log = vec![env(
            0,
            EventV2::MoleculeNucleated {
                molecule_id: m.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: Vec::new(),
            },
        )];
        let mut probe = std::collections::HashMap::new();
        probe.insert(m.clone(), true);
        let report = audit_trace(&log, &ConstProbe(probe));
        assert_eq!(report.drifts.len(), 1, "{:?}", report.drifts);
        match &report.drifts[0] {
            Drift::BypassMerge { molecule_id, .. } => {
                assert_eq!(molecule_id, &m);
            }
            other => panic!("expected BypassMerge, got {other:?}"),
        }
    }

    #[test]
    fn disabled_action_fires_when_merge_without_complete() {
        // Scenario: MergeCompleted arrives without a preceding
        // MoleculeCompleted. Spec::Done requires status=Completed.
        let m = mid("cs-20260419-cccc");
        let log = vec![
            env(
                0,
                EventV2::MoleculeNucleated {
                    molecule_id: m.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
            ),
            env(
                1,
                EventV2::MergeCompleted {
                    molecule: m.clone(),
                    branch: "feat/task-y".into(),
                    result: MergeResult::Ok,
                    federation_provenance: None,
                },
            ),
        ];
        let report = audit_trace(&log, &NullProbe);
        assert_eq!(report.drifts.len(), 1);
        match &report.drifts[0] {
            Drift::DisabledActionFired {
                action,
                molecule_id,
                ..
            } => {
                assert_eq!(*action, Action::Done);
                assert_eq!(molecule_id.as_ref(), Some(&m));
            }
            other => panic!("expected DisabledActionFired, got {other:?}"),
        }
    }

    #[test]
    fn audit_report_serialises_to_json() {
        let m = mid("cs-20260419-dddd");
        let report = AuditReport {
            drifts: vec![Drift::BypassMerge {
                seq: Seq(42),
                molecule_id: m,
            }],
            events_replayed: 1,
            molecules_seen: 1,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"kind\":\"bypass_merge\""));
        assert!(json.contains("\"seq\":42"));
        let back: AuditReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }
}
