// SPDX-License-Identifier: AGPL-3.0-only

//! JSONL event log persistence.
//!
//! Plain functions (per ADR-COS-001) that append [`Envelope`] values to a
//! newline-delimited JSON file. Each call opens the file in append mode,
//! writes one JSON object followed by `\n`, and closes the handle.
//!
//! # Examples
//!
//! ```no_run
//! use cosmon_core::event::{Envelope, Event};
//! use cosmon_core::id::WorkerId;
//! use std::path::Path;
//!
//! let path = Path::new("/tmp/cosmon-events.jsonl");
//! let envelope = Envelope::now(Event::WorkerSpawned {
//!     worker_id: WorkerId::new("quartz").unwrap(),
//!     agent: "polecat".to_owned(),
//! });
//! cosmon_filestore::event::append(path, &envelope).unwrap();
//! ```

use std::fs::OpenOptions;
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::path::Path;

use cosmon_core::error::CosmonError;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::message::MessagePriority;

/// Append a single event envelope as one JSON line to the given file.
///
/// Creates the file (and parent directories) if they do not exist.
/// Each invocation opens the file in append mode, writes one compact JSON
/// object followed by a newline, and flushes.
///
/// # Errors
///
/// Returns [`CosmonError::Io`] on filesystem failures or
/// [`CosmonError::Json`] if serialization fails.
pub fn append(path: &Path, envelope: &Envelope) -> Result<(), CosmonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Chain linkage (plumbing v2, Month 1): every appended envelope carries
    // `prev_hash` pointing at the previous entry and `hash` committing to
    // its own canonical form. Old files without hashes still load — this
    // function just starts hashing from the next append onward.
    let prev_hash = tail_hash(path)?;
    let mut sealed = envelope.clone();
    sealed.prev_hash = prev_hash;
    sealed.hash = None;
    let payload = sealed.hash_payload();
    let h = cosmon_hash::hash_value(&payload).map_err(|e| {
        CosmonError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("canonical hash: {e}"),
        ))
    })?;
    sealed.hash = Some(h);

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_vec(&sealed)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

/// Read the `hash` field of the last non-empty line in a JSONL file.
///
/// Returns `None` if the file is absent, empty, or the tail entry predates
/// plumbing v2 (no `hash` field). Callers treat `None` as "start of chain".
fn tail_hash(path: &Path) -> Result<Option<cosmon_hash::Hash>, CosmonError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)?;
    let Some(last) = content.lines().rev().find(|l| !l.is_empty()) else {
        return Ok(None);
    };
    let env: Envelope = match serde_json::from_str(last) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    Ok(env.hash)
}

/// A divergence in the hash chain discovered by [`verify_chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainDivergence {
    /// Zero-based index of the first non-matching entry.
    pub index: usize,
    /// Why the entry failed to verify.
    pub reason: ChainDivergenceReason,
}

/// Reasons an event log can fail `cs verify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainDivergenceReason {
    /// Entry's stored `prev_hash` does not match the preceding entry's `hash`.
    PrevHashMismatch {
        /// What the entry claimed its predecessor was.
        expected: Option<cosmon_hash::Hash>,
        /// What the preceding entry's own hash actually is.
        actual: Option<cosmon_hash::Hash>,
    },
    /// Entry's stored `hash` does not match its recomputed canonical hash.
    HashMismatch {
        /// What the file claims.
        stored: cosmon_hash::Hash,
        /// What we recomputed.
        recomputed: cosmon_hash::Hash,
    },
    /// Entry is missing its `hash` field (pre-v2 legacy or truncated write).
    MissingHash,
    /// EventV2 log: the monotone `seq` witness regressed — an entry's
    /// sequence number is not strictly greater than its predecessor's.
    ///
    /// EventV2 envelopes (the schema the current writer emits) carry no
    /// hash chain; their tamper-evidence is the strictly increasing
    /// per-file `seq`. A non-increasing `seq` means the log was
    /// reordered, spliced, or had a line removed.
    SeqRegression {
        /// The predecessor's `seq`; the walk requires strictly greater.
        expected_gt: u64,
        /// The `seq` actually found on this entry.
        found: u64,
    },
}

/// Outcome of walking a JSONL chain with [`verify_chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Every entry hashed correctly and chained to its predecessor.
    Verified {
        /// Number of entries walked.
        count: usize,
    },
    /// Log is entirely pre-v2 (no entries carry a `hash`). Not an error,
    /// but `cs verify` surfaces it as a warning.
    UnsignedLegacy {
        /// Number of entries walked.
        count: usize,
    },
    /// First point at which the chain diverges.
    Diverged(ChainDivergence),
}

/// Walk the event log and verify its integrity, selecting the check that
/// matches the on-disk schema.
///
/// Cosmon's event log has evolved through two wire schemas, and a molecule
/// that has been tackled/run carries the *newer* one:
///
/// - **Legacy `Envelope`** (`kind` discriminator, plumbing v2) carries a
///   BLAKE3 hash chain (`prev_hash`/`hash`). Verified `git fsck`-style:
///   recompute each entry's canonical hash and check it links to its
///   predecessor.
/// - **[`EventV2`](cosmon_core::event_v2::Envelope)** (`type` discriminator,
///   the schema the current writer emits) carries **no** hash chain — its
///   tamper-evidence is the strictly increasing per-file `seq`. Verified by
///   walking `seq` and demanding strict monotonicity.
///
/// Before this split the walker deserialised every line into the legacy
/// `Envelope` and rejected the whole log with `missing field 'kind'` the
/// moment it hit an EventV2 line — which meant `cs verify` failed on every
/// honestly-produced molecule (tester Jesse Thaler, issue #1). The schema
/// is now detected: a file whose every line parses as legacy `Envelope`
/// walks the hash chain; otherwise it is an EventV2 log and walks `seq`.
///
/// The first divergence wins — we stop there so operators see a single,
/// actionable line.
///
/// # Errors
///
/// Returns [`CosmonError::Io`] on I/O failures or [`CosmonError::Json`]
/// if a line cannot be parsed as either schema.
pub fn verify_chain(path: &Path) -> Result<VerifyOutcome, CosmonError> {
    if !path.exists() {
        return Ok(VerifyOutcome::Verified { count: 0 });
    }
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(VerifyOutcome::Verified { count: 0 });
    }

    // Schema detection. EventV2 lines use a `type` discriminator and never
    // parse as a legacy `Envelope` (which needs `kind`), so a file whose
    // every line deserialises as `Envelope` is a legacy hash-chained log;
    // anything else is treated as the current EventV2 schema.
    let legacy: Result<Vec<Envelope>, _> = lines
        .iter()
        .map(|l| serde_json::from_str::<Envelope>(l))
        .collect();
    match legacy {
        Ok(entries) => verify_legacy_hash_chain(&entries),
        Err(_) => verify_eventv2_seq_chain(&lines),
    }
}

/// Walk the legacy plumbing-v2 BLAKE3 hash chain (the original
/// [`verify_chain`] behaviour, now schema-gated).
fn verify_legacy_hash_chain(entries: &[Envelope]) -> Result<VerifyOutcome, CosmonError> {
    let signed = entries.iter().filter(|e| e.hash.is_some()).count();
    if signed == 0 {
        return Ok(VerifyOutcome::UnsignedLegacy {
            count: entries.len(),
        });
    }

    let mut prev_hash: Option<cosmon_hash::Hash> = None;
    for (idx, env) in entries.iter().enumerate() {
        let Some(stored) = env.hash else {
            return Ok(VerifyOutcome::Diverged(ChainDivergence {
                index: idx,
                reason: ChainDivergenceReason::MissingHash,
            }));
        };
        if env.prev_hash != prev_hash {
            return Ok(VerifyOutcome::Diverged(ChainDivergence {
                index: idx,
                reason: ChainDivergenceReason::PrevHashMismatch {
                    expected: env.prev_hash,
                    actual: prev_hash,
                },
            }));
        }
        let mut bare = env.clone();
        bare.hash = None;
        let recomputed = cosmon_hash::hash_value(&bare.hash_payload()).map_err(|e| {
            CosmonError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("canonical hash: {e}"),
            ))
        })?;
        if recomputed != stored {
            return Ok(VerifyOutcome::Diverged(ChainDivergence {
                index: idx,
                reason: ChainDivergenceReason::HashMismatch { stored, recomputed },
            }));
        }
        prev_hash = Some(stored);
    }
    Ok(VerifyOutcome::Verified {
        count: entries.len(),
    })
}

/// Walk an [`EventV2`](cosmon_core::event_v2::Envelope) log and verify the
/// strict monotonicity of its per-file `seq` witness.
///
/// EventV2 envelopes carry no hash chain, so `seq` is the integrity signal:
/// the writer assigns a strictly increasing sequence under `flock`, and a
/// reader can prove no line was reordered or dropped by walking it upward.
///
/// Lines that only coerce through *legacy* migration (a mixed grace-window
/// log) do not carry an authoritative `seq` — [`migrate_legacy_line`] stamps
/// them `Seq(0)` — so, matching the writer's own reader semantics
/// (`cosmon_state::event_log`), they are tolerated but do not contribute to
/// sequencing. A line that is neither native EventV2 nor coercible legacy is
/// genuine corruption and surfaces as a parse error.
///
/// [`migrate_legacy_line`]: cosmon_core::event_v2::migrate_legacy_line
fn verify_eventv2_seq_chain(lines: &[&str]) -> Result<VerifyOutcome, CosmonError> {
    use cosmon_core::event_v2::Envelope as EventV2Envelope;

    let mut prev_seq: Option<u64> = None;
    for (idx, line) in lines.iter().enumerate() {
        match serde_json::from_str::<EventV2Envelope>(line) {
            Ok(env) => {
                let seq = env.seq.0;
                if let Some(prev) = prev_seq {
                    if seq <= prev {
                        return Ok(VerifyOutcome::Diverged(ChainDivergence {
                            index: idx,
                            reason: ChainDivergenceReason::SeqRegression {
                                expected_gt: prev,
                                found: seq,
                            },
                        }));
                    }
                }
                prev_seq = Some(seq);
            }
            // Not native EventV2 — tolerate a genuine legacy line, but let a
            // line that is neither shape surface as a parse error.
            Err(_) => {
                EventV2Envelope::from_line(line)?;
            }
        }
    }
    Ok(VerifyOutcome::Verified { count: lines.len() })
}

/// Read all event envelopes from a JSONL file.
///
/// Returns an empty `Vec` if the file does not exist.
///
/// # Errors
///
/// Returns [`CosmonError::Io`] on read failures or [`CosmonError::Json`]
/// if any line fails to deserialize.
pub fn read_all(path: &Path) -> Result<Vec<Envelope>, CosmonError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)?;
    let mut envelopes = Vec::new();
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let envelope: Envelope = serde_json::from_str(line)?;
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

/// Result of a [`poll_events`] call: new events plus the updated byte offset.
#[derive(Debug, Clone)]
pub struct PollResult {
    /// Events read since the previous offset.
    pub events: Vec<Envelope>,
    /// Byte offset to pass to the next call (points past the last complete line).
    pub offset: u64,
}

/// Read only the events appended since the given byte offset.
///
/// This is the incremental counterpart to [`read_all`]: callers persist the
/// returned `offset` between calls so each poll returns only new events.
/// Passing `offset = 0` is equivalent to `read_all` (reads everything).
///
/// If the file does not exist, returns an empty result with `offset = 0`.
/// If the file has been truncated below `offset` (e.g. log rotation),
/// reads from the beginning instead of returning stale data.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use cosmon_filestore::event::poll_events;
///
/// let path = Path::new("/tmp/cosmon-events.jsonl");
/// let result = poll_events(path, 0).unwrap();
/// // Next time, pass result.offset to get only new events.
/// let result2 = poll_events(path, result.offset).unwrap();
/// ```
///
/// # Errors
///
/// Returns [`CosmonError::Io`] on filesystem failures or
/// [`CosmonError::Json`] if any new line fails to deserialize.
pub fn poll_events(path: &Path, offset: u64) -> Result<PollResult, CosmonError> {
    if !path.exists() {
        return Ok(PollResult {
            events: Vec::new(),
            offset: 0,
        });
    }

    let mut file = OpenOptions::new().read(true).open(path)?;

    // If the file shrank (log rotation), restart from beginning.
    let file_len = file.metadata()?.len();
    let start = if offset > file_len { 0 } else { offset };

    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }

    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    let mut events = Vec::new();
    let mut pos = 0usize;
    let bytes = buf.as_bytes();
    while pos < bytes.len() {
        // Find the next newline.
        let Some(nl) = memchr_newline(bytes, pos) else {
            // No newline found — partial line at EOF, don't consume.
            break;
        };
        let line = &buf[pos..nl];
        pos = nl + 1; // skip past '\n'
        if line.is_empty() {
            continue;
        }
        let envelope: Envelope = serde_json::from_str(line)?;
        events.push(envelope);
    }

    Ok(PollResult {
        events,
        offset: start + pos as u64,
    })
}

/// Append a [`Event::TaskDispatched`] event to the JSONL log.
///
/// This is the JSONL-channel path for task dispatch: when [`select_channel`]
/// returns [`Channel::JsonlFile`](cosmon_core::message::Channel::JsonlFile), the caller creates the durable record
/// by appending this event instead of creating a Dolt Bead.
///
/// # Errors
///
/// Returns [`CosmonError::Io`] on filesystem failures or
/// [`CosmonError::Json`] if serialization fails.
///
/// [`select_channel`]: cosmon_core::message::select_channel
pub fn send_task_event(
    path: &Path,
    title: &str,
    target: &str,
    priority: MessagePriority,
    bead_id: Option<String>,
) -> Result<(), CosmonError> {
    let channel = cosmon_core::message::select_channel(priority);
    let envelope = Envelope::now(Event::TaskDispatched {
        title: title.to_owned(),
        target: target.to_owned(),
        priority,
        channel,
        bead_id,
    });
    append(path, &envelope)
}

/// Filter [`TaskDispatched`](Event::TaskDispatched) events from a poll result.
///
/// Convenience function for callers who poll the JSONL channel and only
/// care about dispatched tasks (ignoring worker lifecycle events, etc.).
#[must_use]
pub fn filter_task_dispatched(events: &[Envelope]) -> Vec<&Envelope> {
    events
        .iter()
        .filter(|e| matches!(e.event, Event::TaskDispatched { .. }))
        .collect()
}

/// Find the index of the next `\n` byte starting from `start`.
fn memchr_newline(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| start + i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event::Event;
    use cosmon_core::id::{MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use tempfile::TempDir;

    fn events_path(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("events.jsonl")
    }

    #[test]
    fn test_append_creates_file_and_writes_one_line() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let envelope = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            agent: "polecat".to_owned(),
        });
        append(&path, &envelope).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let back: Envelope = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back.event, envelope.event);
        assert!(back.hash.is_some(), "append must seal with a hash");
        assert!(back.prev_hash.is_none(), "first entry has no predecessor");
    }

    #[test]
    fn test_append_multiple_events() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("quartz").unwrap(),
            agent: "polecat".to_owned(),
        });
        let e2 = Envelope::now(Event::WorkerTerminated {
            worker_id: WorkerId::new("quartz").unwrap(),
            reason: "done".to_owned(),
        });
        let e3 = Envelope::now(Event::MoleculeDispatched {
            molecule_id: MoleculeId::new("cs-20260401-abcd").unwrap(),
            worker_id: WorkerId::new("onyx").unwrap(),
        });

        append(&path, &e1).unwrap();
        append(&path, &e2).unwrap();
        append(&path, &e3).unwrap();

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), 3);
        assert_eq!(envelopes[0].event, e1.event);
        assert_eq!(envelopes[1].event, e2.event);
        assert_eq!(envelopes[2].event, e3.event);
        // Hash chain linkage.
        assert!(envelopes[0].prev_hash.is_none());
        assert_eq!(envelopes[1].prev_hash, envelopes[0].hash);
        assert_eq!(envelopes[2].prev_hash, envelopes[1].hash);
    }

    #[test]
    fn test_read_all_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let result = read_all(&path).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_append_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("deep/nested/events.jsonl");

        let envelope = Envelope::now(Event::ErrorOccurred {
            context: "test".to_owned(),
            message: "boom".to_owned(),
        });
        append(&path, &envelope).unwrap();

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].event, envelope.event);
    }

    #[test]
    fn test_all_event_variants_through_jsonl() {
        use cosmon_core::event::MutationType;
        use cosmon_core::id::AgentId;

        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let events = vec![
            Event::WorkerSpawned {
                worker_id: WorkerId::new("a").unwrap(),
                agent: "x".to_owned(),
            },
            Event::WorkerTerminated {
                worker_id: WorkerId::new("a").unwrap(),
                reason: "exit".to_owned(),
            },
            Event::MoleculeDispatched {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                worker_id: WorkerId::new("a").unwrap(),
            },
            Event::MoleculeTransitioned {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                from: MoleculeStatus::Running,
                to: MoleculeStatus::Completed,
            },
            Event::StepCompleted {
                molecule_id: MoleculeId::new("cs-20260401-aaaa").unwrap(),
                step: 0,
                total: 3,
            },
            Event::IntentDeclared {
                agent_id: AgentId::new("witness").unwrap(),
                target_domain: "molecules".to_owned(),
                mutation_type: MutationType::Create,
                expected_scope: "nucleate new molecule".to_owned(),
            },
            Event::ErrorOccurred {
                context: "ctx".to_owned(),
                message: "msg".to_owned(),
            },
        ];

        for evt in &events {
            let envelope = Envelope::now(evt.clone());
            append(&path, &envelope).unwrap();
        }

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), events.len());
        for (envelope, expected) in envelopes.iter().zip(events.iter()) {
            assert_eq!(&envelope.event, expected);
        }
    }

    // ── send_task_event tests ────────────────────────────────────────

    #[test]
    fn test_send_task_event_appends_task_dispatched() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        send_task_event(
            &path,
            "fix widget",
            "cosmon/polecats/jasper",
            cosmon_core::message::MessagePriority::Normal,
            None,
        )
        .unwrap();

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), 1);
        match &envelopes[0].event {
            cosmon_core::event::Event::TaskDispatched {
                title,
                target,
                priority,
                channel,
                bead_id,
            } => {
                assert_eq!(title, "fix widget");
                assert_eq!(target, "cosmon/polecats/jasper");
                assert_eq!(*priority, cosmon_core::message::MessagePriority::Normal);
                assert_eq!(*channel, cosmon_core::message::Channel::SignalBus);
                assert!(bead_id.is_none());
            }
            other => panic!("expected TaskDispatched, got {other:?}"),
        }
    }

    #[test]
    fn test_send_task_event_with_bead_id() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        send_task_event(
            &path,
            "critical fix",
            "cosmon/polecats/opal",
            cosmon_core::message::MessagePriority::Critical,
            Some("cs-abc".to_owned()),
        )
        .unwrap();

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), 1);
        match &envelopes[0].event {
            cosmon_core::event::Event::TaskDispatched {
                channel, bead_id, ..
            } => {
                assert_eq!(*channel, cosmon_core::message::Channel::DoltBead);
                assert_eq!(bead_id.as_deref(), Some("cs-abc"));
            }
            other => panic!("expected TaskDispatched, got {other:?}"),
        }
    }

    #[test]
    fn test_filter_task_dispatched() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        // Mix of event types
        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        append(&path, &e1).unwrap();

        send_task_event(
            &path,
            "task one",
            "cosmon/polecats/ruby",
            cosmon_core::message::MessagePriority::Low,
            None,
        )
        .unwrap();

        let e3 = Envelope::now(Event::ErrorOccurred {
            context: "ctx".to_owned(),
            message: "msg".to_owned(),
        });
        append(&path, &e3).unwrap();

        send_task_event(
            &path,
            "task two",
            "cosmon/polecats/opal",
            cosmon_core::message::MessagePriority::Normal,
            None,
        )
        .unwrap();

        let all = read_all(&path).unwrap();
        assert_eq!(all.len(), 4);

        let tasks = filter_task_dispatched(&all);
        assert_eq!(tasks.len(), 2);
    }

    // ── poll_events tests ──────────────────────────────────────────

    #[test]
    fn test_poll_events_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.jsonl");
        let result = poll_events(&path, 0).unwrap();
        assert!(result.events.is_empty());
        assert_eq!(result.offset, 0);
    }

    #[test]
    fn test_poll_events_from_zero_reads_all() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        let e2 = Envelope::now(Event::WorkerTerminated {
            worker_id: WorkerId::new("a").unwrap(),
            reason: "done".to_owned(),
        });
        append(&path, &e1).unwrap();
        append(&path, &e2).unwrap();

        let result = poll_events(&path, 0).unwrap();
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0].event, e1.event);
        assert_eq!(result.events[1].event, e2.event);
        assert!(result.offset > 0);
    }

    #[test]
    fn test_poll_events_incremental() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        append(&path, &e1).unwrap();

        // First poll: read e1.
        let r1 = poll_events(&path, 0).unwrap();
        assert_eq!(r1.events.len(), 1);
        assert_eq!(r1.events[0].event, e1.event);

        // Append more events.
        let e2 = Envelope::now(Event::WorkerTerminated {
            worker_id: WorkerId::new("a").unwrap(),
            reason: "exit".to_owned(),
        });
        let e3 = Envelope::now(Event::ErrorOccurred {
            context: "ctx".to_owned(),
            message: "msg".to_owned(),
        });
        append(&path, &e2).unwrap();
        append(&path, &e3).unwrap();

        // Second poll: only e2 and e3.
        let r2 = poll_events(&path, r1.offset).unwrap();
        assert_eq!(r2.events.len(), 2);
        assert_eq!(r2.events[0].event, e2.event);
        assert_eq!(r2.events[1].event, e3.event);

        // Third poll with no new data: empty.
        let r3 = poll_events(&path, r2.offset).unwrap();
        assert!(r3.events.is_empty());
        assert_eq!(r3.offset, r2.offset);
    }

    #[test]
    fn test_poll_events_handles_truncated_file() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        // Write several events to build up a large offset.
        for i in 0..5 {
            let e = Envelope::now(Event::WorkerSpawned {
                worker_id: WorkerId::new(format!("w-{i}")).unwrap(),
                agent: "polecat".to_owned(),
            });
            append(&path, &e).unwrap();
        }
        let r1 = poll_events(&path, 0).unwrap();
        assert_eq!(r1.events.len(), 5);

        // Simulate log rotation: truncate and write a single new event.
        let e_new = Envelope::now(Event::ErrorOccurred {
            context: "new".to_owned(),
            message: "rotated".to_owned(),
        });
        std::fs::write(&path, "").unwrap(); // truncate
        append(&path, &e_new).unwrap();

        // Old offset is well beyond new file length — should reset to 0.
        let r2 = poll_events(&path, r1.offset).unwrap();
        assert_eq!(r2.events.len(), 1);
        assert_eq!(r2.events[0].event, e_new.event);
    }

    #[test]
    fn test_poll_events_skips_partial_line() {
        use std::io::Write as _;

        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        append(&path, &e1).unwrap();

        // Simulate a partial write (no trailing newline).
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"partial\":true").unwrap();

        let result = poll_events(&path, 0).unwrap();
        // Should read only e1, ignoring the incomplete line.
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].event, e1.event);
    }

    // ── verify_chain tests ───────────────────────────────────────────

    #[test]
    fn test_verify_chain_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        let outcome = verify_chain(&path).unwrap();
        assert_eq!(outcome, VerifyOutcome::Verified { count: 0 });
    }

    #[test]
    fn test_verify_chain_valid_chain() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        let e2 = Envelope::now(Event::WorkerTerminated {
            worker_id: WorkerId::new("a").unwrap(),
            reason: "done".to_owned(),
        });
        append(&path, &e1).unwrap();
        append(&path, &e2).unwrap();

        let outcome = verify_chain(&path).unwrap();
        assert_eq!(outcome, VerifyOutcome::Verified { count: 2 });
    }

    #[test]
    fn test_verify_chain_detects_tamper() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        let e1 = Envelope::now(Event::WorkerSpawned {
            worker_id: WorkerId::new("a").unwrap(),
            agent: "x".to_owned(),
        });
        append(&path, &e1).unwrap();

        // Tamper: rewrite the first line with a different agent name but
        // keep the original hash — the recomputed hash won't match.
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replace(r#""agent":"x"#, r#""agent":"y"#);
        std::fs::write(&path, &tampered).unwrap();

        let outcome = verify_chain(&path).unwrap();
        assert!(matches!(outcome, VerifyOutcome::Diverged(_)));
    }

    #[test]
    fn test_verify_chain_legacy_unsigned() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        // Write a raw line without hash fields (pre-v2 format).
        let legacy = r#"{"timestamp":"2026-01-01T00:00:00Z","kind":"worker_spawned","worker_id":"a","agent":"x"}"#;
        std::fs::write(&path, format!("{legacy}\n")).unwrap();

        let outcome = verify_chain(&path).unwrap();
        assert_eq!(outcome, VerifyOutcome::UnsignedLegacy { count: 1 });
    }

    // ── EventV2-schema chain tests (issue #1 regression) ─────────────

    /// Serialize an EventV2 envelope exactly as the live writer does — one
    /// compact JSON object with a `seq`/`emitter_kind` header and a `type`
    /// discriminator, no `kind`, no hash chain.
    fn write_v2_line(path: &std::path::Path, env: &cosmon_core::event_v2::Envelope) {
        use std::io::Write as _;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let line = serde_json::to_string(env).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn v2_completed(seq: u64, mol: &str) -> cosmon_core::event_v2::Envelope {
        use cosmon_core::event_v2::{Envelope, EventV2, Seq};
        use cosmon_core::id::MoleculeId;
        Envelope::new(
            Seq(seq),
            None,
            EventV2::MoleculeCompleted {
                molecule_id: MoleculeId::new(mol).unwrap(),
                duration_ms: None,
                reason: "done".to_owned(),
            },
        )
    }

    #[test]
    fn test_verify_chain_eventv2_log_passes() {
        // The flagship regression: a molecule that has been tackled has a
        // pure EventV2 events.jsonl (`type`/`emitter_kind`, monotone `seq`,
        // no hash chain). Before the fix this failed with
        // `missing field 'kind'`; it must now PASS.
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);
        for seq in 0..3 {
            write_v2_line(&path, &v2_completed(seq, "task-20260720-ccb5"));
        }
        let outcome = verify_chain(&path).unwrap();
        assert_eq!(outcome, VerifyOutcome::Verified { count: 3 });
    }

    #[test]
    fn test_verify_chain_eventv2_seq_regression_diverges() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);
        // seq 0, 1, then a regression back to 1 (not strictly increasing).
        write_v2_line(&path, &v2_completed(0, "task-20260720-aaaa"));
        write_v2_line(&path, &v2_completed(1, "task-20260720-bbbb"));
        write_v2_line(&path, &v2_completed(1, "task-20260720-cccc"));
        let outcome = verify_chain(&path).unwrap();
        match outcome {
            VerifyOutcome::Diverged(d) => {
                assert_eq!(d.index, 2);
                assert_eq!(
                    d.reason,
                    ChainDivergenceReason::SeqRegression {
                        expected_gt: 1,
                        found: 1
                    }
                );
            }
            other => panic!("expected Diverged(SeqRegression), got {other:?}"),
        }
    }

    #[test]
    fn test_verify_chain_eventv2_line_shape_matches_reader() {
        // Belt-and-suspenders: the on-disk line the writer produces round
        // trips through the same EventV2 reader the walker now uses.
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);
        write_v2_line(&path, &v2_completed(0, "task-20260720-dddd"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let line = raw.lines().next().unwrap();
        assert!(line.contains(r#""type":"molecule_completed""#));
        assert!(line.contains(r#""emitter_kind""#));
        assert!(!line.contains(r#""kind":"#));
        assert!(cosmon_core::event_v2::Envelope::from_line(line).is_ok());
    }

    #[test]
    fn test_backward_compat_legacy_events_load() {
        let dir = TempDir::new().unwrap();
        let path = events_path(&dir);

        // Old format: no prev_hash, no hash.
        let legacy = r#"{"timestamp":"2026-01-01T00:00:00Z","kind":"worker_spawned","worker_id":"a","agent":"x"}"#;
        std::fs::write(&path, format!("{legacy}\n")).unwrap();

        let envelopes = read_all(&path).unwrap();
        assert_eq!(envelopes.len(), 1);
        assert!(envelopes[0].prev_hash.is_none());
        assert!(envelopes[0].hash.is_none());
        assert!(matches!(envelopes[0].event, Event::WorkerSpawned { .. }));
    }
}
