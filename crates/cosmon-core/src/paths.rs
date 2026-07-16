// SPDX-License-Identifier: AGPL-3.0-only

//! `CosmonPath` — the single source of truth for the **relative path layout**
//! that cosmon writes under a state root.
//!
//! # Why this module exists
//!
//! Cosmon used to carry a hand-curated list of "the paths cosmon writes",
//! kept in sync with the call-sites by human memory. That list is a *mirror
//! schema*: the doc re-states the shape that the writers own, and drift is
//! silent until a runtime symptom. The cure is one rule: **one canonical
//! encoding per fact, at the site causally closest to making it true; every
//! other appearance is a decode, never a stored second copy.** The writer
//! is causally closest to the write, so the path-set must be *emitted from
//! the writer*, never listed beside it.
//!
//! This module is that canonical encoding. [`CosmonPathKind`] enumerates the
//! taxonomy (every kind of path cosmon writes, and nothing else);
//! [`CosmonPath`] is the typed, parameterised instance whose [`CosmonPath::rel`]
//! is *the one path computation*. The projection `cs paths --writes` is a
//! pure render of [`CosmonPathKind::iter`] — it reads nothing, keeps no index
//! on disk, and cannot fall stale. A path kind nobody writes is a dead
//! variant (caught by `match` exhaustiveness); a path the taxonomy omits is a
//! kind that simply has no [`CosmonPath`] constructor.
//!
//! # Class (a) vs class (b) — the honest interim
//!
//! The decision criterion: collapse to *class (a)* (the list **is** the
//! type, drift unrepresentable) only if **every** state write already funnels
//! through one chokepoint; otherwise *class (b)* (emit the taxonomy + one
//! consistency test, while migrating toward the chokepoint).
//!
//! Empirically (this task, step 1) cosmon is **class (b)**: state writes are
//! scattered across [`crate`]-sibling crates —
//! `cosmon-filestore` (`FileStore`, `PresenceStore`, `cas`, `event`) **and**
//! `cosmon-state` (`archive`, `event_log`, `rebuild`, `token_meter`). They do
//! *not* all funnel through one `FileStore::write`. ADR-110's
//! *single-writer-trunk* is a discipline on the **git main branch** (the
//! `trunk.lock`), not a filesystem-write chokepoint — so the trunk it names
//! does not make B7 class (a) by itself.
//!
//! Therefore this module lives in **`cosmon-core`**, not in `cosmon-filestore`
//! as first sketched: `cosmon-filestore` *depends on* `cosmon-state`,
//! so a chokepoint enum in `cosmon-filestore` would be unreachable from the
//! `cosmon-state` writers. `cosmon-core` is the one crate both sides already
//! depend on, and [`CosmonPath::rel`] is a pure `PathBuf` computation (no
//! I/O), so it respects the zero-I/O-core rule.
//!
//! The interim discipline: every writer **decodes** its path from
//! [`CosmonPath`] instead of hand-joining strings. As decode-sites are wired
//! in, the taxonomy stops being a parallel doc and becomes load-bearing. The
//! class (a) end-state — a single `FileStore::write(CosmonPath, &[u8])` with
//! no raw `fs::write` to the tree — is the migration target, tracked as the
//! remaining funnel work; the variants not yet wired are listed in
//! [`CosmonPathKind::owner`] so the tail is visible, not pretended away.

use std::path::PathBuf;

use strum::{EnumIter, IntoEnumIterator};

use crate::cas::ContentHash;
use crate::id::{FleetId, MoleculeId, SessionId};

/// The **kind** of a write-path: the taxonomy of where cosmon writes,
/// independent of any concrete molecule/session/hash.
///
/// This is the fieldless, iterable face of the layout. `cs paths --writes`
/// renders [`CosmonPathKind::iter`]; `match` exhaustiveness over this enum is
/// what makes a never-written entry a *dead variant* (a compile error in
/// [`CosmonPathKind::template`]) rather than a stale doc line.
///
/// Every variant has a 1:1 [`CosmonPath`] instance constructor — adding a
/// kind here without a matching instance arm (or vice-versa) is caught by the
/// exhaustive [`CosmonPath::kind`] match and the `template`↔`rel` consistency
/// test in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum CosmonPathKind {
    /// `fleet.json` — durable fleet snapshot (who exists, roles, assignments).
    Fleet,
    /// `fleet.runtime.json` — host-local worker overlay (worktree paths,
    /// restart counters). Gitignored; never crosses a residence boundary.
    FleetRuntime,
    /// `fleet.lock` — advisory lock serialising fleet-state JSON writes.
    FleetLock,
    /// `trunk.lock` — advisory lock serialising **git main trunk** writes
    /// (ADR-110 I1 WRITER-UNIQUE). Distinct from [`Self::FleetLock`].
    TrunkLock,
    /// `events.jsonl` — fleet-level append-only transition stream at the state
    /// root (distinct from the per-molecule [`Self::MoleculeEvents`]).
    FleetEvents,
    /// `fleets/<fleet>/molecules/<mol>/state.json` — authoritative molecule
    /// state (the current layout).
    MoleculeState,
    /// `fleets/<fleet>/molecules/<mol>/events.jsonl` — per-molecule
    /// hash-chained transition log.
    MoleculeEvents,
    /// `ops/molecules/<mol>/state.json` — legacy flat molecule layout, still
    /// read (and migrated on next save) for pre-fleet-split state dirs.
    LegacyMoleculeState,
    /// `presence/<session>.json` — presence registry snapshot for a session.
    PresenceSnapshot,
    /// `presence/<session>.log` — directed-whisper pull channel for a session.
    PresenceLog,
    /// `presence/<session>.seek` — whisper read-offset pointer for a session.
    PresenceSeek,
    /// `cas/<prefix>/<hash>` — content-addressed blob (response hashes, etc.).
    Cas,
    /// `archive/<YYYY>/<MM>/<mol>/` — durable terminal-transition snapshot
    /// directory (ADR-030). Survives worktree teardown.
    ArchiveMolecule,
    /// `archive/events/events-<YYYY-MM>.jsonl` — fleet-level archived
    /// transition stream (ADR-030, append-only).
    ArchiveFleetEvents,
    /// `archive/SCHEMA_VERSION` — archive schema-version marker (ADR-030).
    ArchiveSchemaVersion,
}

/// Whether a write-path is meant to be captured in version control.
///
/// Consumers of the projection (the `.gitignore` stanza, the ADR-030 archive
/// manifest, backup scripts) need this bit so they can be *generated* from the
/// taxonomy instead of hand-typed. It is advisory metadata, not an
/// enforcement: cosmon never `chmod`s a path on the strength of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// Live runtime state under `.cosmon/state/` — gitignored on `main`
    /// (the coarse `.cosmon/state/` glob covers it).
    Gitignored,
    /// An advisory lock file — ephemeral, gitignored, safe to delete.
    Lock,
    /// Durable archive captured on the `cosmon/state` orphan branch so the
    /// chain of reasoning is readable from a fresh clone (ADR-030).
    ArchiveTracked,
}

impl CosmonPathKind {
    /// The relative-path **template** for this kind, with `<…>` placeholders
    /// for the parameterised segments.
    ///
    /// This is the human-readable face emitted by `cs paths --writes`. It is
    /// kept honest by the consistency test: a concrete [`CosmonPath::rel`]
    /// must match this template with every `<…>` token filled.
    #[must_use]
    pub fn template(self) -> &'static str {
        match self {
            Self::Fleet => "fleet.json",
            Self::FleetRuntime => "fleet.runtime.json",
            Self::FleetLock => "fleet.lock",
            Self::TrunkLock => "trunk.lock",
            Self::FleetEvents => "events.jsonl",
            Self::MoleculeState => "fleets/<fleet>/molecules/<mol>/state.json",
            Self::MoleculeEvents => "fleets/<fleet>/molecules/<mol>/events.jsonl",
            Self::LegacyMoleculeState => "ops/molecules/<mol>/state.json",
            Self::PresenceSnapshot => "presence/<session>.json",
            Self::PresenceLog => "presence/<session>.log",
            Self::PresenceSeek => "presence/<session>.seek",
            Self::Cas => "cas/<prefix>/<hash>",
            Self::ArchiveMolecule => "archive/<YYYY>/<MM>/<mol>/",
            Self::ArchiveFleetEvents => "archive/events/events-<YYYY-MM>.jsonl",
            Self::ArchiveSchemaVersion => "archive/SCHEMA_VERSION",
        }
    }

    /// The crate/module that *owns the write* for this kind — the site
    /// causally closest to making the path true (the canonical "source").
    ///
    /// Doubles as the migration ledger: owners in `cosmon-state::*` are the
    /// stray writers not yet funnelled through a [`CosmonPath`] decode-site;
    /// they are honestly listed rather than pretended into class (a).
    #[must_use]
    pub fn owner(self) -> &'static str {
        match self {
            Self::Fleet
            | Self::FleetRuntime
            | Self::FleetLock
            | Self::TrunkLock
            | Self::MoleculeState
            | Self::LegacyMoleculeState => "cosmon-filestore::FileStore",
            // Not yet funnelled: the fleet-level stream is appended from many
            // `cosmon-cli` sites via `resolve_state_dir(None).join(...)`.
            Self::FleetEvents => "cosmon-cli (scattered, migration tail)",
            Self::MoleculeEvents => "cosmon-filestore::event",
            Self::PresenceSnapshot | Self::PresenceLog | Self::PresenceSeek => {
                "cosmon-filestore::PresenceStore"
            }
            Self::Cas => "cosmon-filestore::cas",
            Self::ArchiveMolecule | Self::ArchiveFleetEvents | Self::ArchiveSchemaVersion => {
                "cosmon-state::archive"
            }
        }
    }

    /// One-line description of what this path carries.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Fleet => "durable fleet snapshot (identities, roles, assignments)",
            Self::FleetRuntime => "host-local worker overlay (worktree paths, restart counts)",
            Self::FleetLock => "advisory lock for fleet-state JSON writes",
            Self::TrunkLock => "advisory lock for git main-trunk writes (ADR-110 I1)",
            Self::FleetEvents => "fleet-level append-only transition stream",
            Self::MoleculeState => "authoritative molecule state",
            Self::MoleculeEvents => "per-molecule hash-chained transition log",
            Self::LegacyMoleculeState => "legacy flat molecule state (pre fleet-split)",
            Self::PresenceSnapshot => "presence registry snapshot for a session",
            Self::PresenceLog => "directed-whisper pull channel for a session",
            Self::PresenceSeek => "whisper read-offset pointer for a session",
            Self::Cas => "content-addressed blob (response hashes, attachments)",
            Self::ArchiveMolecule => "durable terminal-transition snapshot (ADR-030)",
            Self::ArchiveFleetEvents => "fleet-level archived transition stream (ADR-030)",
            Self::ArchiveSchemaVersion => "archive schema-version marker (ADR-030)",
        }
    }

    /// Whether this path is gitignored runtime state, an ephemeral lock, or a
    /// durable archive tracked on the orphan branch.
    #[must_use]
    pub fn persistence(self) -> Persistence {
        match self {
            Self::FleetLock | Self::TrunkLock => Persistence::Lock,
            Self::ArchiveMolecule | Self::ArchiveFleetEvents | Self::ArchiveSchemaVersion => {
                Persistence::ArchiveTracked
            }
            _ => Persistence::Gitignored,
        }
    }

    /// Iterate every write-path kind. Re-exported convenience over the
    /// derived [`strum::IntoEnumIterator`] so callers need not import the
    /// trait.
    pub fn all() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

/// A **concrete** write-path: a [`CosmonPathKind`] with its parameters bound.
///
/// This is the typed argument a write chokepoint accepts. [`Self::rel`] is the
/// *one* place a relative cosmon write-path is computed; every `FileStore` /
/// `PresenceStore` path method decodes from here rather than hand-joining
/// segments, so the method and the taxonomy cannot silently diverge.
///
/// The lifetime borrows the id arguments — constructing a `CosmonPath` is
/// allocation-free at the call-site; only [`Self::rel`] allocates a `PathBuf`.
#[derive(Debug, Clone, Copy)]
pub enum CosmonPath<'a> {
    /// See [`CosmonPathKind::Fleet`].
    Fleet,
    /// See [`CosmonPathKind::FleetRuntime`].
    FleetRuntime,
    /// See [`CosmonPathKind::FleetLock`].
    FleetLock,
    /// See [`CosmonPathKind::TrunkLock`].
    TrunkLock,
    /// See [`CosmonPathKind::FleetEvents`].
    FleetEvents,
    /// See [`CosmonPathKind::MoleculeState`].
    MoleculeState {
        /// Owning fleet.
        fleet: &'a FleetId,
        /// Molecule id.
        id: &'a MoleculeId,
    },
    /// See [`CosmonPathKind::MoleculeEvents`].
    MoleculeEvents {
        /// Owning fleet.
        fleet: &'a FleetId,
        /// Molecule id.
        id: &'a MoleculeId,
    },
    /// See [`CosmonPathKind::LegacyMoleculeState`].
    LegacyMoleculeState {
        /// Molecule id.
        id: &'a MoleculeId,
    },
    /// See [`CosmonPathKind::PresenceSnapshot`].
    PresenceSnapshot {
        /// Session id.
        session: &'a SessionId,
    },
    /// See [`CosmonPathKind::PresenceLog`].
    PresenceLog {
        /// Session id.
        session: &'a SessionId,
    },
    /// See [`CosmonPathKind::PresenceSeek`].
    PresenceSeek {
        /// Session id.
        session: &'a SessionId,
    },
    /// See [`CosmonPathKind::Cas`].
    Cas {
        /// Content hash of the blob.
        hash: &'a ContentHash,
    },
    /// See [`CosmonPathKind::ArchiveMolecule`].
    ArchiveMolecule {
        /// Calendar year of the transition.
        year: i32,
        /// Calendar month of the transition (1–12).
        month: u32,
        /// Molecule id.
        id: &'a MoleculeId,
    },
    /// See [`CosmonPathKind::ArchiveFleetEvents`].
    ArchiveFleetEvents {
        /// Calendar year of the stream bucket.
        year: i32,
        /// Calendar month of the stream bucket (1–12).
        month: u32,
    },
    /// See [`CosmonPathKind::ArchiveSchemaVersion`].
    ArchiveSchemaVersion,
}

impl CosmonPath<'_> {
    /// The taxonomy kind this instance belongs to.
    ///
    /// The exhaustive match here is the structural tie between the instance
    /// enum and [`CosmonPathKind`]: a new instance variant cannot compile
    /// without naming its kind, and a removed kind breaks this match.
    #[must_use]
    pub fn kind(&self) -> CosmonPathKind {
        match self {
            Self::Fleet => CosmonPathKind::Fleet,
            Self::FleetRuntime => CosmonPathKind::FleetRuntime,
            Self::FleetLock => CosmonPathKind::FleetLock,
            Self::TrunkLock => CosmonPathKind::TrunkLock,
            Self::FleetEvents => CosmonPathKind::FleetEvents,
            Self::MoleculeState { .. } => CosmonPathKind::MoleculeState,
            Self::MoleculeEvents { .. } => CosmonPathKind::MoleculeEvents,
            Self::LegacyMoleculeState { .. } => CosmonPathKind::LegacyMoleculeState,
            Self::PresenceSnapshot { .. } => CosmonPathKind::PresenceSnapshot,
            Self::PresenceLog { .. } => CosmonPathKind::PresenceLog,
            Self::PresenceSeek { .. } => CosmonPathKind::PresenceSeek,
            Self::Cas { .. } => CosmonPathKind::Cas,
            Self::ArchiveMolecule { .. } => CosmonPathKind::ArchiveMolecule,
            Self::ArchiveFleetEvents { .. } => CosmonPathKind::ArchiveFleetEvents,
            Self::ArchiveSchemaVersion => CosmonPathKind::ArchiveSchemaVersion,
        }
    }

    /// **The one and only** relative-path computation, rooted at the state
    /// directory (the `<root>` a `FileStore` / `PresenceStore` is built on).
    ///
    /// Callers join this onto their state root: `store_root.join(path.rel())`.
    /// No write-site should compute a state-relative path any other way.
    #[must_use]
    pub fn rel(&self) -> PathBuf {
        match self {
            Self::Fleet => PathBuf::from("fleet.json"),
            Self::FleetRuntime => PathBuf::from("fleet.runtime.json"),
            Self::FleetLock => PathBuf::from("fleet.lock"),
            Self::TrunkLock => PathBuf::from("trunk.lock"),
            Self::FleetEvents => PathBuf::from("events.jsonl"),
            Self::MoleculeState { fleet, id } => PathBuf::from("fleets")
                .join(fleet.as_str())
                .join("molecules")
                .join(id.as_str())
                .join("state.json"),
            Self::MoleculeEvents { fleet, id } => PathBuf::from("fleets")
                .join(fleet.as_str())
                .join("molecules")
                .join(id.as_str())
                .join("events.jsonl"),
            Self::LegacyMoleculeState { id } => PathBuf::from("ops")
                .join("molecules")
                .join(id.as_str())
                .join("state.json"),
            Self::PresenceSnapshot { session } => {
                PathBuf::from("presence").join(format!("{}.json", session.as_str()))
            }
            Self::PresenceLog { session } => {
                PathBuf::from("presence").join(format!("{}.log", session.as_str()))
            }
            Self::PresenceSeek { session } => {
                PathBuf::from("presence").join(format!("{}.seek", session.as_str()))
            }
            Self::Cas { hash } => PathBuf::from("cas").join(hash.prefix()).join(hash.as_str()),
            Self::ArchiveMolecule { year, month, id } => PathBuf::from("archive")
                .join(format!("{year:04}"))
                .join(format!("{month:02}"))
                .join(id.as_str()),
            Self::ArchiveFleetEvents { year, month } => PathBuf::from("archive")
                .join("events")
                .join(format!("events-{year:04}-{month:02}.jsonl")),
            Self::ArchiveSchemaVersion => PathBuf::from("archive").join("SCHEMA_VERSION"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative concrete instance for every kind — the fixture that
    /// ties the parameterised instance enum back to the fieldless taxonomy.
    fn sample(kind: CosmonPathKind) -> CosmonPath<'static> {
        // Leak small ids so the sample borrows can be `'static` in the test.
        fn fleet() -> &'static FleetId {
            Box::leak(Box::new(FleetId::new("default").unwrap()))
        }
        fn mol() -> &'static MoleculeId {
            Box::leak(Box::new(MoleculeId::new("task-20260607-7f58").unwrap()))
        }
        fn session() -> &'static SessionId {
            Box::leak(Box::new(SessionId::new("sess-abc").unwrap()))
        }
        fn hash() -> &'static ContentHash {
            // 64 lowercase hex chars (SHA-256 shape).
            Box::leak(Box::new(
                ContentHash::new("a".repeat(64)).expect("valid sha256 hex"),
            ))
        }
        match kind {
            CosmonPathKind::Fleet => CosmonPath::Fleet,
            CosmonPathKind::FleetRuntime => CosmonPath::FleetRuntime,
            CosmonPathKind::FleetLock => CosmonPath::FleetLock,
            CosmonPathKind::TrunkLock => CosmonPath::TrunkLock,
            CosmonPathKind::FleetEvents => CosmonPath::FleetEvents,
            CosmonPathKind::MoleculeState => CosmonPath::MoleculeState {
                fleet: fleet(),
                id: mol(),
            },
            CosmonPathKind::MoleculeEvents => CosmonPath::MoleculeEvents {
                fleet: fleet(),
                id: mol(),
            },
            CosmonPathKind::LegacyMoleculeState => CosmonPath::LegacyMoleculeState { id: mol() },
            CosmonPathKind::PresenceSnapshot => CosmonPath::PresenceSnapshot { session: session() },
            CosmonPathKind::PresenceLog => CosmonPath::PresenceLog { session: session() },
            CosmonPathKind::PresenceSeek => CosmonPath::PresenceSeek { session: session() },
            CosmonPathKind::Cas => CosmonPath::Cas { hash: hash() },
            CosmonPathKind::ArchiveMolecule => CosmonPath::ArchiveMolecule {
                year: 2026,
                month: 6,
                id: mol(),
            },
            CosmonPathKind::ArchiveFleetEvents => CosmonPath::ArchiveFleetEvents {
                year: 2026,
                month: 6,
            },
            CosmonPathKind::ArchiveSchemaVersion => CosmonPath::ArchiveSchemaVersion,
        }
    }

    /// Every kind round-trips: `sample(kind).kind() == kind`. Guarantees the
    /// instance enum and the taxonomy enum stay in 1:1 correspondence.
    #[test]
    fn every_kind_has_an_instance() {
        for kind in CosmonPathKind::all() {
            assert_eq!(sample(kind).kind(), kind, "instance for {kind:?}");
        }
    }

    /// The `template` placeholders are the *only* difference between the
    /// kind's template and a concrete `rel`: filling every `<…>` segment of
    /// the template with the sample's bound value reproduces `rel` exactly.
    /// This is the load-bearing consistency check — it makes the two faces of
    /// the layout (taxonomy template ↔ instance computation) impossible to
    /// drift apart silently.
    #[test]
    fn template_segments_match_rel_segments() {
        for kind in CosmonPathKind::all() {
            let rel = sample(kind).rel();
            let rel_str = rel.to_string_lossy();
            let template = kind.template();
            // Same number of segments (trailing slash on dir templates is
            // cosmetic — strip it before counting).
            let tmpl_segments: Vec<&str> = template.trim_end_matches('/').split('/').collect();
            let rel_segments: Vec<&str> = rel_str.split('/').collect();
            assert_eq!(
                tmpl_segments.len(),
                rel_segments.len(),
                "segment count mismatch for {kind:?}: template={template:?} rel={rel_str:?}"
            );
            // Literal segments must be byte-identical; *partially*-templated
            // segments (e.g. `events-<YYYY-MM>.jsonl`) must honour their
            // literal prefix and suffix around the `<…>` placeholder.
            for (t, r) in tmpl_segments.iter().zip(rel_segments.iter()) {
                if t.contains('<') {
                    let pre = &t[..t.find('<').unwrap()];
                    let post = &t[t.rfind('>').unwrap() + 1..];
                    assert!(
                        r.starts_with(pre) && r.ends_with(post),
                        "templated segment {t:?} not honoured by rel segment {r:?} for {kind:?}"
                    );
                } else {
                    assert_eq!(
                        t, r,
                        "literal segment mismatch for {kind:?}: template={template:?} rel={rel_str:?}"
                    );
                }
            }
        }
    }

    /// Snapshot of the full projected write-path set. If a kind is added,
    /// removed, or its template changes, this golden list must be updated in
    /// the same change — the diff is the review surface that replaces the old
    /// hand-maintained P1 list (now generated, never typed).
    #[test]
    fn write_path_taxonomy_snapshot() {
        let got: Vec<&'static str> = CosmonPathKind::all()
            .map(CosmonPathKind::template)
            .collect();
        let expected = vec![
            "fleet.json",
            "fleet.runtime.json",
            "fleet.lock",
            "trunk.lock",
            "events.jsonl",
            "fleets/<fleet>/molecules/<mol>/state.json",
            "fleets/<fleet>/molecules/<mol>/events.jsonl",
            "ops/molecules/<mol>/state.json",
            "presence/<session>.json",
            "presence/<session>.log",
            "presence/<session>.seek",
            "cas/<prefix>/<hash>",
            "archive/<YYYY>/<MM>/<mol>/",
            "archive/events/events-<YYYY-MM>.jsonl",
            "archive/SCHEMA_VERSION",
        ];
        assert_eq!(
            got, expected,
            "write-path taxonomy drifted from the golden snapshot"
        );
    }
}
