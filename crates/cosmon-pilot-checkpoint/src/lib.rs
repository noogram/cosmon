// SPDX-License-Identifier: AGPL-3.0-only

//! Checkpoint publication and tri-valued drift comparison — M3 of the
//! *co-pilotage multi-provider* mission.
//!
//! # The one question this crate answers
//!
//! *When two pilots have been flying the same mission, what can you say about
//! their disagreement that you can also prove?*
//!
//! The tempting answer is a similarity score, and it is the wrong one. A number
//! between 0 and 1 cannot be argued with, cannot be checked, and quietly
//! becomes an authority nobody granted it. So this crate says one of exactly
//! three things, and it says it by citation:
//!
//! | Verdict | What it means | When it can be said |
//! |---|---|---|
//! | [`Verdict::Finding`] | a decidable test fired | both checkpoints exist, and both sides of the disagreement are quoted |
//! | [`Verdict::Agree`] | the compared positions match | both checkpoints exist **and** something was actually comparable |
//! | [`Verdict::Inconclusive`] | it could not be compared | anything else — a missing checkpoint, a different mission, nothing in common |
//!
//! The third row is the load-bearing one. Mission falsifier 8 is *"a missing
//! checkpoint is rendered as `AGREE`"*, and the way this crate makes that
//! unfalsifiable is structural: [`compare`] returns before it can reach any
//! agreement code path when a checkpoint is absent, and
//! [`DriftReport`]'s roll-up treats an empty finding list as `INCONCLUSIVE`.
//!
//! # Shape
//!
//! ```text
//!   CheckpointStore ──publish──▶ <state>/pilot/checkpoints/<mission>/<id>.json
//!         │  load / latest_for
//!         ▼
//!   PilotCheckpoint × 2 ──compare(a, b, now)──▶ DriftReport
//!                                                 ├ verdict: AGREE | FINDING | INCONCLUSIVE
//!                                                 └ findings: [DriftFinding { class, cited_claims, … }]
//! ```
//!
//! The comparison is I/O-free and takes its clock as a parameter; the store is
//! the only module that opens a file. That is what lets every rule below be
//! tested without a tempdir, and it is the domain-core discipline the
//! contributor guide asks for.
//!
//! # What is deliberately absent
//!
//! - **No CLI verb, no flag, no output byte.** `cs sessions` is M5. This crate
//!   is a library, like `cosmon-session-probe` before it.
//! - **No score, no confidence, no distance.** ADR-168 D3.4. A finding cites
//!   two assertions and their evidence, or it is `INCONCLUSIVE`.
//! - **No conversation content.** A checkpoint is a compact hand-over record,
//!   not a transcript — CHECKPOINT-NOT-SCROLLBACK, and the confidentiality
//!   ceiling ADR-168 sets for the whole mission.
//! - **No authority.** ADVISORY-DRIFT: a report advises. Nothing here mutates a
//!   molecule, grants a lease, or has an API that could.
//!
//! # Example — the co-pilot never published, so nothing is agreed
//!
//! ```
//! use chrono::{DateTime, Utc};
//! use cosmon_pilot_checkpoint::{
//!     compare, Claim, CheckpointStore, DriftClass, EvidenceRef, MissionId, PilotCheckpoint,
//!     SessionId, Stance, Verdict,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let tmp = tempfile::tempdir()?;
//! let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("in range");
//! let store = CheckpointStore::new(tmp.path());
//!
//! let mut primary = PilotCheckpoint::new("cp-a", "task-20260731-67f2", "sess-claude", 1, now)?;
//! primary.intended_next_actions.push(
//!     Claim::new("i1", "merge-to-main", Stance::Affirm, "merge once gates are green")?
//!         .with_evidence([EvidenceRef::new("docs/adr/168.md")]),
//! );
//! store.publish(&primary)?;
//!
//! let mission = MissionId::new("task-20260731-67f2")?;
//! let copilot = store.latest_for(&mission, &SessionId::new("sess-codex")?)?;
//! assert!(copilot.is_none());
//!
//! let report = compare(Some(&primary), copilot.as_ref(), now);
//! assert_eq!(report.verdict, Verdict::Inconclusive);
//! assert!(report.has_class(DriftClass::MissingCheckpoint));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod checkpoint;
pub mod drift;
pub mod error;
pub mod id;
pub mod store;

pub use checkpoint::{Claim, EvidenceRef, PilotCheckpoint, Scope, Stance};
pub use drift::{compare, CitedClaim, DriftClass, DriftFinding, DriftReport, Side, Verdict};
pub use error::CheckpointError;
pub use id::{CheckpointId, ClaimId, FindingId, MissionId, SessionId};
pub use store::CheckpointStore;
