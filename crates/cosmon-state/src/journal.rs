// SPDX-License-Identifier: AGPL-3.0-only

//! The per-molecule journal — a **projection** of the galaxy ledger.
//!
//! A molecule has no journal file. It has a journal *view*, computed on
//! demand by folding `.cosmon/state/events.jsonl` — the one canonical,
//! append-only ledger — down to the rows that name this molecule. Nothing in
//! this module writes anything.
//!
//! ## Why a projection and not a file
//!
//! The obvious implementation is a second `events.jsonl` inside the molecule
//! directory, written alongside the galaxy one. Cosmon has already paid for
//! that shape once: the root-spawn refusal recorder opened **two** sinks, and
//! the second sink is what made the refusal look recorded when it was not
//! (ADR-166, COSMON-DEV #20). Two writers means two truths, and the truth an
//! operator reads is whichever one happened to be writable.
//!
//! One writer makes that whole class inexpressible. The ledger is written by
//! [`crate::event_log::EventLogWriter`] and by nothing else; this module only
//! reads. The five clauses of the contract fall out of that single choice
//! rather than needing five mechanisms:
//!
//! | clause | how it holds |
//! |---|---|
//! | projection of the ledger, never a second file | there is no second file — [`MoleculeJournal::project`] is a pure fold |
//! | exists from nucleation | `molecule_nucleated` is appended by `cs nucleate`, so the view is non-empty from that instant, with no file to create |
//! | contains blockages where the worker produced nothing | a refusal is a ledger row like any other; see [`JournalEntry::is_blockage`] |
//! | survives teardown and archival | the ledger lives in `.cosmon/state/`, not in the worktree `cs done` destroys; the archive additionally materialises a rendered copy |
//! | mechanically reconstructible | the view *is* the reconstruction; `project` is a total function of the ledger bytes |
//!
//! The "exists from nucleation" clause deserves the sharpest statement,
//! because the natural way to satisfy it is the wrong one. Creating a file at
//! nucleation would mean a root dispatcher creates a root-owned file on a
//! galaxy whose worker uid is not root — precisely the residue ADR-166
//! refuses. A view that is computed rather than stored exists from nucleation
//! *and* performs zero writes, so there is nothing for a privileged process to
//! leave behind.
//!
//! ## Reading at the JSON level, not the typed level
//!
//! [`EventV2`](cosmon_core::event_v2::EventV2) is `#[non_exhaustive]` and
//! serde-tagged: a row whose `type` this binary does not know fails to
//! deserialize. The refusal row (`tackle_refused`) is exactly such a row — it
//! is written as raw JSON by `cs tackle`, deliberately, before any typed
//! machinery exists. A projection that only saw typed events would drop the
//! one entry the operator most needs.
//!
//! So the fold works on `serde_json::Value`. Every row that names the
//! molecule appears in the journal, including rows from a newer writer than
//! the reader. The journal's job is to lose nothing, not to interpret
//! everything.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use cosmon_core::id::MoleculeId;

/// Field names, in probe order, under which a ledger row may carry the
/// molecule it is about.
///
/// The ledger's vocabulary is not uniform — the typed events use
/// `molecule_id`, the spawn-seam family uses `mol_id`, the merge family uses
/// `molecule`, a decay splice records the parent it replaced under `parent`,
/// and an operator spark points at `mol_ref`. Mirrors
/// [`EventV2::molecule_id`](cosmon_core::event_v2::EventV2::molecule_id),
/// which is the typed statement of the same table, but reaches rows that type
/// cannot parse.
const MOLECULE_KEYS: [&str; 5] = ["molecule_id", "mol_id", "molecule", "parent", "mol_ref"];

/// Row types that record the molecule **failing to advance**.
///
/// The operator's contract calls these "blockages where the worker produced
/// nothing", and they are the reason the journal exists: a molecule that
/// completed leaves artefacts to read, while a molecule refused on its first
/// dispatch leaves only ledger rows.
///
/// Two rows earn their place here by history rather than by symmetry:
/// `tackle_refused` is the typed root-spawn refusal of ADR-166, and
/// `worker_spawn_failed` is its untyped neighbour. The `sf1`…`sf7` structured
/// failure family is matched by prefix in [`is_blockage_type`] instead of
/// being enumerated, so a new `sf8` classifies correctly the day it lands.
const BLOCKAGE_TYPES: [&str; 12] = [
    "tackle_refused",
    "worker_spawn_failed",
    "worker_spawn_rolled_back",
    "molecule_collapsed",
    "molecule_stuck",
    "expired",
    "gate_failed",
    "native_failed",
    "worker_silence_detected",
    "worker_blocked_on_operator",
    "blocking_dialogue_detected",
    "external_channel_timeout",
];

/// Does this row type record a blockage rather than progress?
///
/// ```
/// use cosmon_state::journal::is_blockage_type;
/// assert!(is_blockage_type("tackle_refused"));
/// assert!(is_blockage_type("sf5_context_overflow"));
/// assert!(!is_blockage_type("molecule_nucleated"));
/// ```
#[must_use]
pub fn is_blockage_type(event_type: &str) -> bool {
    BLOCKAGE_TYPES.contains(&event_type)
        || (event_type.len() > 3
            && event_type.starts_with("sf")
            && event_type.as_bytes()[2].is_ascii_digit())
}

/// One ledger row, as the journal sees it.
///
/// [`Self::raw`] is the row verbatim, so nothing the ledger recorded is lost
/// in projection; the other fields are conveniences lifted out of it for
/// rendering and for callers that want to filter without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Global ledger sequence, when the row carries one. Legacy rows written
    /// before the sequencing writer landed carry none.
    pub seq: Option<u64>,
    /// Per-molecule sequence, when the row carries one.
    pub mol_seq: Option<u64>,
    /// RFC3339 timestamp as recorded, unparsed — the journal reports what the
    /// ledger says rather than re-deriving it.
    pub timestamp: Option<String>,
    /// The row's `type` tag, or `"unknown"` when a row carries none.
    pub event_type: String,
    /// Whether this row records the molecule failing to advance.
    pub is_blockage: bool,
    /// The row verbatim.
    pub raw: Value,
}

impl JournalEntry {
    /// Lift one ledger row into an entry, or `None` if it is not about
    /// `molecule`.
    fn from_row(row: Value, molecule: &MoleculeId) -> Option<Self> {
        if !row_names(&row, molecule) {
            return None;
        }
        let event_type = row
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        Some(Self {
            seq: row.get("seq").and_then(Value::as_u64),
            mol_seq: row.get("mol_seq").and_then(Value::as_u64),
            timestamp: row
                .get("timestamp")
                .or_else(|| row.get("ts"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            is_blockage: is_blockage_type(&event_type),
            event_type,
            raw: row,
        })
    }
}

/// Does this row name `molecule` in any of the ledger's molecule fields?
fn row_names(row: &Value, molecule: &MoleculeId) -> bool {
    MOLECULE_KEYS.iter().any(|key| {
        row.get(*key)
            .and_then(Value::as_str)
            .is_some_and(|id| id == molecule.as_str())
    })
}

/// The per-molecule journal: every ledger row naming one molecule, in ledger
/// order.
///
/// Construct it with [`Self::project`] (pure, I/O-free — the domain core owes
/// no filesystem) or with [`Self::project_from_state_dir`], which reads the
/// galaxy ledger and delegates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoleculeJournal {
    /// The molecule this journal is about.
    pub molecule_id: MoleculeId,
    /// The rows naming it, in the order the ledger records them.
    ///
    /// Ledger order, not sorted order: the ledger is append-only under a
    /// `flock`, so file order *is* causal order, and it is the only order
    /// that survives rows which carry no `seq`.
    pub entries: Vec<JournalEntry>,
}

impl MoleculeJournal {
    /// Project a journal out of ledger lines. Pure: no I/O, no allocation of
    /// state beyond the returned value.
    ///
    /// Unparseable lines are skipped rather than failing the projection. A
    /// ledger with one torn line at the tail — the shape a crash during append
    /// produces — must still yield the rest of the molecule's history, because
    /// the history is what the operator came for.
    ///
    /// ```
    /// use cosmon_core::id::MoleculeId;
    /// use cosmon_state::journal::MoleculeJournal;
    ///
    /// let id = MoleculeId::new("task-20260730-7a74").expect("well-formed id");
    /// let ledger = r#"{"seq":1,"type":"molecule_nucleated","molecule_id":"task-20260730-7a74","formula_id":"task-work"}
    /// {"seq":2,"type":"molecule_nucleated","molecule_id":"task-19700101-0000","formula_id":"task-work"}
    /// {"seq":3,"type":"tackle_refused","molecule_id":"task-20260730-7a74","reason":"root-spawn-refused:demote-shares-repository-storage"}"#;
    ///
    /// let journal = MoleculeJournal::project(ledger.lines(), &id);
    /// assert_eq!(journal.entries.len(), 2);
    /// assert_eq!(journal.blockages().count(), 1);
    /// ```
    #[must_use]
    pub fn project<'a>(lines: impl IntoIterator<Item = &'a str>, molecule: &MoleculeId) -> Self {
        let entries = lines
            .into_iter()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .filter_map(|row| JournalEntry::from_row(row, molecule))
            .collect();
        Self {
            molecule_id: molecule.clone(),
            entries,
        }
    }

    /// Project from a galaxy state directory, i.e. from
    /// `<state_dir>/events.jsonl`.
    ///
    /// A missing ledger yields an empty journal rather than an error: a galaxy
    /// with no ledger has recorded nothing about any molecule, which is a fact
    /// about the galaxy and not a failure of this call. Any other read error
    /// is returned, because "the ledger is unreadable" and "the ledger says
    /// nothing" must never render identically.
    ///
    /// This is the only function in the module that touches the filesystem,
    /// and it opens the ledger read-only. Projecting a journal on a galaxy
    /// leaves it byte-identical — the property
    /// `projecting_a_journal_leaves_the_galaxy_byte_identical` pins.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when the ledger exists but cannot
    /// be read (permissions, a truncated read, invalid UTF-8). A ledger that
    /// is simply absent is not an error — see above.
    pub fn project_from_state_dir(state_dir: &Path, molecule: &MoleculeId) -> io::Result<Self> {
        let path = crate::event_log::resolve_events_log_path(state_dir);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Self::project(text.lines(), molecule)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self {
                molecule_id: molecule.clone(),
                entries: Vec::new(),
            }),
            Err(err) => Err(err),
        }
    }

    /// The rows recording a blockage — where the molecule did not advance.
    pub fn blockages(&self) -> impl Iterator<Item = &JournalEntry> {
        self.entries.iter().filter(|entry| entry.is_blockage)
    }

    /// Timestamp of the `molecule_nucleated` row, when the ledger has one.
    ///
    /// Its presence is the machine-checkable form of "the journal exists from
    /// nucleation": a molecule that has been nucleated has this row, and
    /// therefore a non-empty journal, before any worker exists.
    #[must_use]
    pub fn nucleated_at(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.event_type == "molecule_nucleated")
            .and_then(|entry| entry.timestamp.as_deref())
    }

    /// Render as JSONL — the projected rows verbatim, one per line.
    ///
    /// Byte-deterministic for a given ledger: `serde_json::Value` keeps object
    /// keys in a `BTreeMap`, so re-serialisation is key-sorted and stable.
    /// This is the form the reconstructibility test compares.
    #[must_use]
    pub fn render_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(&entry.raw.to_string());
            out.push('\n');
        }
        out
    }

    /// Render as Markdown, for an operator reading one molecule's history.
    ///
    /// Blockages are marked so the case the primitive exists for — a molecule
    /// that produced nothing — reads at a glance instead of requiring the
    /// reader to know which type tags mean failure.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = format!("# Journal — {}\n\n", self.molecule_id.as_str());
        out.push_str(
            "Projection of the galaxy ledger (`.cosmon/state/events.jsonl`). Not a stored file:\n\
             regenerate it at any time with `cs events journal <molecule-id>`.\n\n",
        );
        if self.entries.is_empty() {
            out.push_str(
                "The ledger records nothing about this molecule. If it was nucleated in this\n\
                 galaxy, the ledger is the thing to look at — not this view.\n",
            );
            return out;
        }
        // `write!` into a `String` is infallible; the `Result` is discarded
        // deliberately rather than propagated, so this stays a total render.
        let _ = writeln!(
            out,
            "{} row(s), {} blockage(s).\n",
            self.entries.len(),
            self.blockages().count()
        );
        for entry in &self.entries {
            let mark = if entry.is_blockage { "⛔" } else { "·" };
            let when = entry.timestamp.as_deref().unwrap_or("(no timestamp)");
            let _ = writeln!(out, "{mark} {when}  {}", entry.event_type);
            if let Some(detail) = blockage_detail(entry) {
                let _ = writeln!(out, "    {detail}");
            }
        }
        out
    }
}

/// The human-readable cause carried by a blockage row, when it carries one.
///
/// Kept to the two fields the fleet's blockage rows actually populate
/// (`reason`, then `detail`) rather than dumping the row: an operator reading
/// a refusal wants the sentence, and the row itself is one `--json` away.
fn blockage_detail(entry: &JournalEntry) -> Option<String> {
    if !entry.is_blockage {
        return None;
    }
    let reason = entry.raw.get("reason").and_then(Value::as_str);
    let detail = entry.raw.get("detail").and_then(Value::as_str);
    match (reason, detail) {
        (Some(r), Some(d)) if r != d => Some(format!("{r} — {d}")),
        (Some(r), _) => Some(r.to_owned()),
        (None, Some(d)) => Some(d.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> String {
        [
            r#"{"seq":1,"mol_seq":1,"timestamp":"2026-07-30T08:00:00Z","type":"molecule_nucleated","molecule_id":"task-20260730-aaaa","formula_id":"task-work"}"#,
            r#"{"seq":2,"timestamp":"2026-07-30T08:00:01Z","type":"operator_present","sid":"s1"}"#,
            r#"{"seq":3,"mol_seq":1,"timestamp":"2026-07-30T08:00:02Z","type":"molecule_nucleated","molecule_id":"task-20260730-bbbb","formula_id":"task-work"}"#,
            r#"{"type":"tackle_refused","molecule_id":"task-20260730-aaaa","reason":"root-spawn-refused:demote-shares-repository-storage","detail":"run as uid 501"}"#,
            "not json at all",
            r#"{"seq":5,"type":"worker_spawned","molecule":"task-20260730-aaaa","worker_id":"wkr-1"}"#,
        ]
        .join("\n")
    }

    #[test]
    fn a_journal_holds_only_rows_naming_its_molecule() {
        let journal = MoleculeJournal::project(
            ledger().lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        let types: Vec<&str> = journal
            .entries
            .iter()
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(
            types,
            vec!["molecule_nucleated", "tackle_refused", "worker_spawned"],
            "the fleet-scoped row and the sibling molecule's row must not appear"
        );
    }

    #[test]
    fn a_torn_line_does_not_cost_the_rest_of_the_history() {
        // The unparseable line sits between the refusal and the spawn; both
        // sides of it survive.
        let journal = MoleculeJournal::project(
            ledger().lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        assert_eq!(journal.entries.len(), 3);
    }

    #[test]
    fn the_refusal_row_is_classified_as_a_blockage_and_names_its_cause() {
        let journal = MoleculeJournal::project(
            ledger().lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        let blockages: Vec<&JournalEntry> = journal.blockages().collect();
        assert_eq!(blockages.len(), 1);
        assert_eq!(blockages[0].event_type, "tackle_refused");
        assert_eq!(
            blockage_detail(blockages[0]).as_deref(),
            Some("root-spawn-refused:demote-shares-repository-storage — run as uid 501")
        );
    }

    #[test]
    fn a_molecule_that_was_only_nucleated_already_has_a_journal() {
        // The clause "exists from nucleation": one ledger row, no worker, no
        // molecule directory, no file created — and the view is non-empty.
        let ledger = r#"{"seq":1,"timestamp":"2026-07-30T08:00:00Z","type":"molecule_nucleated","molecule_id":"task-20260730-aaaa","formula_id":"task-work"}"#;
        let journal = MoleculeJournal::project(
            ledger.lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(journal.nucleated_at(), Some("2026-07-30T08:00:00Z"));
    }

    #[test]
    fn rows_stay_in_ledger_order_even_when_seq_is_absent() {
        // The refusal row carries no `seq`. Sorting on `seq` would move it;
        // ledger order keeps it where the ledger put it.
        let journal = MoleculeJournal::project(
            ledger().lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        assert_eq!(journal.entries[1].event_type, "tackle_refused");
        assert!(journal.entries[1].seq.is_none());
    }

    #[test]
    fn projection_is_reconstructible_from_the_ledger_alone() {
        // The claim "mechanically reconstructible" made executable: render the
        // view, throw it away, rebuild it from the ledger, compare bytes.
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        fs::write(state.join("events.jsonl"), ledger()).unwrap();
        let id = MoleculeId::new("task-20260730-aaaa").expect("well-formed id");

        let first = MoleculeJournal::project_from_state_dir(state, &id).unwrap();
        let view = state.join("journal.jsonl");
        fs::write(&view, first.render_jsonl()).unwrap();
        let before = fs::read(&view).unwrap();

        fs::remove_file(&view).unwrap();
        assert!(!view.exists());

        let rebuilt = MoleculeJournal::project_from_state_dir(state, &id).unwrap();
        fs::write(&view, rebuilt.render_jsonl()).unwrap();
        assert_eq!(before, fs::read(&view).unwrap());
        assert_eq!(first, rebuilt);
    }

    #[test]
    fn projecting_a_journal_leaves_the_galaxy_byte_identical() {
        // The residue property of ADR-166, applied to this primitive: the view
        // is computed, so there is nothing for a privileged process to leave
        // behind. A design that created a file at nucleation would fail here.
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        fs::write(state.join("events.jsonl"), ledger()).unwrap();

        let snapshot = |root: &Path| -> Vec<(String, Vec<u8>)> {
            let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(root)
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        fs::read(e.path()).unwrap_or_default(),
                    )
                })
                .collect();
            out.sort();
            out
        };

        let before = snapshot(state);
        let _ = MoleculeJournal::project_from_state_dir(
            state,
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        )
        .unwrap();
        let _ = MoleculeJournal::project_from_state_dir(
            state,
            &MoleculeId::new("task-20260730-cccc").expect("well-formed id"),
        )
        .unwrap();
        assert_eq!(before, snapshot(state));
    }

    #[test]
    fn a_galaxy_with_no_ledger_projects_an_empty_journal_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let journal = MoleculeJournal::project_from_state_dir(
            dir.path(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        )
        .unwrap();
        assert!(journal.entries.is_empty());
        assert!(journal.nucleated_at().is_none());
    }

    #[test]
    fn markdown_marks_the_blockage_and_prints_its_cause() {
        let journal = MoleculeJournal::project(
            ledger().lines(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        let md = journal.render_markdown();
        assert!(md.contains("# Journal — task-20260730-aaaa"));
        assert!(md.contains("⛔"));
        assert!(md.contains("root-spawn-refused:demote-shares-repository-storage"));
        assert!(md.contains("3 row(s), 1 blockage(s)."));
    }

    #[test]
    fn an_empty_journal_says_so_instead_of_rendering_a_bare_header() {
        let journal = MoleculeJournal::project(
            std::iter::empty(),
            &MoleculeId::new("task-20260730-aaaa").expect("well-formed id"),
        );
        assert!(journal.render_markdown().contains("records nothing"));
    }

    #[test]
    fn the_structured_failure_family_classifies_by_prefix() {
        assert!(is_blockage_type("sf1_http_transport"));
        assert!(is_blockage_type("sf7_binary_version_mismatch"));
        assert!(!is_blockage_type("sfx_not_a_failure_code"));
    }
}
