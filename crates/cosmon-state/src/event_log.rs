// SPDX-License-Identifier: AGPL-3.0-only

//! Append-only `EventV2` log — the durable sensor record for fleet replay.
//!
//! The log lives at `.cosmon/state/events.jsonl`: one JSON envelope per line,
//! appended under a POSIX `flock(2)` advisory lock with `O_APPEND` so
//! concurrent writers do not interleave. Every line is a
//! [`cosmon_core::event_v2::Envelope`]; legacy (pre-V2) lines that historically
//! mixed `type` and `kind` fields remain parseable through
//! [`cosmon_core::event_v2::Envelope::from_line`].
//!
//! ## Concurrency model (ADR-052 invariant I7 / Gödel G5)
//!
//! Every append is a closed transaction: open the file with `O_APPEND`,
//! `flock(LOCK_EX)` it, catch the cache up to the current end-of-file by
//! reading only new bytes, write exactly one `\n`-terminated line, drop the
//! lock. Lock acquisition is hybrid: a sub-second non-blocking spin handles
//! momentary collisions cheaply; under sustained N-way contention the
//! writer falls through to a blocking `lock_exclusive`, where the kernel
//! queues waiters in arrival order and prevents the lockstep starvation
//! that pure spin-retry exhibits at 10+ concurrent writers.
//!
//! The writer caches a *delta cursor* (`scanned_to`) plus the running
//! `next_seq` and a per-molecule `mol_seqs` table. Under the lock the
//! writer reads only the bytes appended by other processes since its last
//! emit — keeping the critical section to *O(new bytes)* rather than
//! *O(file size)*. This is what lets the 10-way stress test fit inside the
//! 500 ms lock budget.
//!
//! ## Why both `seq` and `mol_seq`
//!
//! - `seq` is monotone **per file**. It anchors `causal_parent` references
//!   and gives readers a single stream order.
//! - `mol_seq` is monotone **per molecule** (when the event carries one).
//!   It is the in-band witness ADR-052 §I7 requires — a verifier can prove
//!   strict ordering for a single molecule by filtering on `molecule_id`
//!   and walking `mol_seq` upward, without trusting the global stream order.
//!
//! ## Reading
//!
//! - [`read_all`] returns every coercible envelope in file order.
//! - Legacy lines are tolerated (best-effort coerced via
//!   [`cosmon_core::event_v2::Envelope::from_line`]); they do not contribute
//!   to sequencing.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{EmitterKind, Envelope, EventV2, Seq};
use cosmon_core::id::MoleculeId;
use cosmon_core::quality_band::{self, QualityBand};
use fs2::FileExt;

/// Environment variable carrying the emitter kind for the current
/// process — read by [`EventLogWriter::open`] when no explicit emitter
/// has been set via [`EventLogWriter::set_emitter`].
///
/// Wire form is the `EmitterKind` `snake_case` (`cli`, `worker`, …).
/// Unknown values collapse to [`EmitterKind::Unknown`].
pub const EMITTER_KIND_ENV: &str = "COSMON_EMITTER_KIND";

/// Environment variable carrying the emitter id (opaque scoped string,
/// e.g. `worker:wkr-abc123`).
pub const EMITTER_ID_ENV: &str = "COSMON_EMITTER_ID";

/// Environment variable carrying the meta-level (informative; the
/// causal filter on `emitter_kind` is the structural guard).
pub const META_LEVEL_ENV: &str = "COSMON_META_LEVEL";

/// Sub-second budget for the non-blocking spin path. After this window we
/// fall through to a blocking `lock_exclusive` so the kernel can queue
/// waiters fairly and avoid starvation under sustained contention.
const LOCK_FAST_BUDGET: Duration = Duration::from_millis(20);

/// Sleep between successive `try_lock_exclusive` attempts in the fast-spin
/// window. Short enough to be sub-perceptible, long enough to avoid
/// burning a core hammering the kernel.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(1);

/// An append-only writer for `events.jsonl` that assigns monotone sequence
/// numbers under a `flock(2)` advisory lock.
///
/// Every [`Self::emit`] call is a self-contained transaction:
/// `flock(LOCK_EX)` → catch-up scan from the cached cursor → append →
/// unlock. The writer is safe to use from multiple processes against the
/// same `events.jsonl`; the kernel serialises the appends and the
/// catch-up scan keeps the global and per-molecule sequences truthful even
/// if a concurrent writer landed bytes in between our calls.
///
/// Drop the writer or call [`Self::sync`] to flush the OS buffers.
pub struct EventLogWriter {
    path: PathBuf,
    file: File,
    /// Next global sequence to assign. Updated under the lock from the
    /// catch-up scan plus our own write.
    next_seq: Seq,
    /// Per-molecule next-sequence cache. Populated lazily from the
    /// catch-up scan; missing entries default to `Seq(0)`.
    mol_seqs: HashMap<MoleculeId, Seq>,
    /// Byte offset up to which we have scanned the file. Catch-up reads
    /// start here on every emit.
    scanned_to: u64,
    /// UTC timestamp of the most recently observed cs verb emission
    /// ([`EventV2::is_verb`]) — populated from the catch-up scan and
    /// from our own writes. Drives the latency input of the Kahneman K1
    /// [`QualityBand`] computation. `None` until the first verb is seen.
    last_verb_ts: Option<DateTime<Utc>>,
    /// Sticky emitter header forward-filled onto every envelope this
    /// writer produces (cosmon-ward §F1).
    ///
    /// Initialised from the `COSMON_EMITTER_KIND` / `COSMON_EMITTER_ID`
    /// / `COSMON_META_LEVEL` environment variables at [`Self::open`]
    /// time, defaulting to [`EmitterKind::Unknown`] / `""` / `0` when
    /// they are unset. Callers can override per-emit via
    /// [`Self::emit_with_emitter`] or stickily via
    /// [`Self::set_emitter`].
    emitter: EmitterHeader,
}

/// The (`kind`, `id`, `meta_level`) triple that travels on every envelope
/// emitted by an [`EventLogWriter`].
///
/// Stored as a small struct so callers can pass / override / clone it
/// cheaply. Construct via [`EmitterHeader::new`] or
/// [`EmitterHeader::from_env`]; defaults to `(Unknown, "", 0)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmitterHeader {
    /// Coarse role of the writer.
    pub kind: EmitterKind,
    /// Opaque scoped id (`worker:…`, `cli:…`, …).
    pub id: String,
    /// Reflexivity depth (informative).
    pub meta_level: u8,
}

impl EmitterHeader {
    /// Fully-qualified constructor.
    #[must_use]
    pub fn new(kind: EmitterKind, id: impl Into<String>, meta_level: u8) -> Self {
        Self {
            kind,
            id: id.into(),
            meta_level,
        }
    }

    /// Build from the canonical `COSMON_EMITTER_KIND` / `COSMON_EMITTER_ID`
    /// / `COSMON_META_LEVEL` environment variables. Missing or unparseable
    /// values fall back to the documented defaults
    /// ([`EmitterKind::Unknown`] / `""` / `0`).
    #[must_use]
    pub fn from_env() -> Self {
        let kind = std::env::var(EMITTER_KIND_ENV)
            .ok()
            .map(|s| EmitterKind::from_wire(&s))
            .unwrap_or_default();
        let id = std::env::var(EMITTER_ID_ENV).unwrap_or_default();
        let meta_level = std::env::var(META_LEVEL_ENV)
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        Self {
            kind,
            id,
            meta_level,
        }
    }
}

impl EventLogWriter {
    /// Open (or create) the event log at `path` and prime the sequence
    /// caches from the existing tail.
    ///
    /// The parent directory must exist.
    ///
    /// # Errors
    ///
    /// Returns `std::io::Error` if the file cannot be opened or scanned.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;

        let mut writer = Self {
            path,
            file,
            next_seq: Seq(0),
            mol_seqs: HashMap::new(),
            scanned_to: 0,
            last_verb_ts: None,
            emitter: EmitterHeader::from_env(),
        };
        // Prime the caches by walking everything currently on disk. This
        // is the only O(file size) read we perform; subsequent emits read
        // only the delta.
        writer.catch_up_scan()?;
        Ok(writer)
    }

    /// Replace the sticky emitter header used by subsequent calls to
    /// [`Self::emit`].
    ///
    /// Callers that know their emitter at writer-construction time
    /// should prefer setting `COSMON_EMITTER_KIND` / `COSMON_EMITTER_ID`
    /// / `COSMON_META_LEVEL` in the environment before
    /// [`Self::open`] — the env-var path also propagates to subprocesses.
    pub fn set_emitter(&mut self, emitter: EmitterHeader) {
        self.emitter = emitter;
    }

    /// The emitter header currently forward-filled by [`Self::emit`].
    #[must_use]
    pub fn emitter(&self) -> &EmitterHeader {
        &self.emitter
    }

    /// The path the writer is attached to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sequence number the writer expects to assign next.
    ///
    /// This is a *hint*. Under cross-process contention the actual emitted
    /// sequence may be larger because [`Self::emit`] catches the cache up
    /// to the on-disk tail under the lock before assigning.
    #[must_use]
    pub fn next_seq(&self) -> Seq {
        self.next_seq
    }

    /// Emit one event, consuming a sequence number.
    ///
    /// Acquires `flock(LOCK_EX)` on the underlying file (non-blocking, with
    /// a sub-second retry spin), reads any bytes written by other writers
    /// since our last emit, assigns global and per-molecule sequence
    /// numbers from the refreshed cache, writes a single `\n`-terminated
    /// line, and drops the lock.
    ///
    /// # Errors
    ///
    /// - `ErrorKind::InvalidData` if the envelope cannot be serialised.
    /// - Any underlying I/O error from the read/write/lock syscalls.
    pub fn emit(&mut self, event: EventV2, causal_parent: Option<Seq>) -> std::io::Result<Seq> {
        let emitter = self.emitter.clone();
        self.emit_with_emitter(event, causal_parent, &emitter)
    }

    /// Emit one event with an explicit emitter header — does **not**
    /// disturb the writer's sticky [`Self::emitter`].
    ///
    /// Use when a single call site needs to override the writer's
    /// default classification (e.g. a CLI command that proxies a
    /// formula-step emission).
    ///
    /// # Errors
    ///
    /// Same as [`Self::emit`].
    pub fn emit_with_emitter(
        &mut self,
        event: EventV2,
        causal_parent: Option<Seq>,
        emitter: &EmitterHeader,
    ) -> std::io::Result<Seq> {
        acquire_lock(&self.file)?;
        let result = self.emit_locked(event, causal_parent, emitter);
        // Best-effort unlock — the kernel will release on FD close even if
        // this fails, so there is nothing useful to surface to the caller.
        let _ = FileExt::unlock(&self.file);
        result
    }

    fn emit_locked(
        &mut self,
        event: EventV2,
        causal_parent: Option<Seq>,
        emitter: &EmitterHeader,
    ) -> std::io::Result<Seq> {
        // Read any bytes another writer landed since our last emit.
        self.catch_up_scan()?;

        let global_seq = self.next_seq;
        let mol_seq = event
            .molecule_id()
            .map(|m| *self.mol_seqs.get(m).unwrap_or(&Seq(0)));

        // Kahneman K1 — compute the band only on cs verb events. The
        // signals available at this hot path are the operator's local
        // hour and the latency since the most recently observed verb.
        let is_verb = event.is_verb();
        let now = Utc::now();
        let quality_band = is_verb.then(|| compute_band_for(now, self.last_verb_ts));

        let env = Envelope::with_emitter(
            global_seq,
            mol_seq,
            causal_parent,
            quality_band,
            emitter.kind,
            emitter.id.clone(),
            emitter.meta_level,
            event,
        );
        let mut line = serde_json::to_string(&env)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        let written = line.len() as u64;
        self.file.write_all(line.as_bytes())?;

        // Reflect our own append into the caches without re-reading.
        self.next_seq = global_seq.next();
        if let Some(m) = env.event.molecule_id() {
            let next = mol_seq
                .expect("event has molecule_id ⇒ mol_seq is Some")
                .next();
            self.mol_seqs.insert(m.clone(), next);
        }
        if is_verb {
            self.last_verb_ts = Some(env.timestamp);
        }
        self.scanned_to += written;

        Ok(global_seq)
    }

    /// Read any bytes appended past `scanned_to` and fold them into the
    /// in-memory caches. Called from `open` (full file) and from
    /// `emit_locked` (delta only).
    fn catch_up_scan(&mut self) -> std::io::Result<()> {
        let end = self.file.metadata()?.len();
        if end < self.scanned_to {
            // File was truncated or rotated. Reset and rescan from the
            // beginning so we never assign a duplicate sequence.
            self.next_seq = Seq(0);
            self.mol_seqs.clear();
            self.scanned_to = 0;
            self.last_verb_ts = None;
        }
        if end == self.scanned_to {
            return Ok(());
        }

        self.file.seek(SeekFrom::Start(self.scanned_to))?;
        let delta_len = usize::try_from(end - self.scanned_to).unwrap_or(0);
        let mut delta = Vec::with_capacity(delta_len);
        self.file.read_to_end(&mut delta)?;
        // Restore the seek to end so the next append lands at the tail.
        self.file.seek(SeekFrom::End(0))?;

        // Walk the delta byte-by-byte, absorbing only *complete* lines
        // (those followed by a `\n`). A trailing partial line — from a
        // still-in-flight write by an external writer — stays in the
        // file; we will pick it up on the next catch-up.
        let mut cursor: usize = 0;
        while let Some(rel) = delta[cursor..].iter().position(|b| *b == b'\n') {
            let line_end = cursor + rel; // byte index of the '\n'
            let line_bytes = &delta[cursor..line_end];
            cursor = line_end + 1; // advance past the '\n' itself

            if line_bytes.is_empty() {
                continue;
            }
            let Ok(line) = std::str::from_utf8(line_bytes) else {
                continue; // non-UTF-8 — skip without failing the scan
            };
            let trimmed = line.trim_end_matches('\r');
            if trimmed.is_empty() {
                continue;
            }
            let Ok(envelope) = serde_json::from_str::<Envelope>(trimmed) else {
                // Legacy or unrecognised line — does not contribute to
                // canonical sequencing.
                continue;
            };
            let candidate_global = envelope.seq.next();
            if candidate_global > self.next_seq {
                self.next_seq = candidate_global;
            }
            if let (Some(mol), Some(ms)) = (envelope.event.molecule_id(), envelope.mol_seq) {
                let candidate_mol = ms.next();
                let entry = self.mol_seqs.entry(mol.clone()).or_insert(Seq(0));
                if candidate_mol > *entry {
                    *entry = candidate_mol;
                }
            }
            // Track the latest verb-event timestamp so the K1 latency
            // signal survives writer restarts and cross-process emits.
            if envelope.event.is_verb()
                && self
                    .last_verb_ts
                    .is_none_or(|prev| envelope.timestamp > prev)
            {
                self.last_verb_ts = Some(envelope.timestamp);
            }
        }
        self.scanned_to += cursor as u64;
        Ok(())
    }

    /// Flush and fsync the underlying file.
    ///
    /// # Errors
    ///
    /// Returns `std::io::Error` on flush or sync failure.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.sync_data()
    }
}

/// Convenience — open the writer, emit one event, sync, and drop.
///
/// Useful for CLI commands that only emit a single event per invocation.
/// The emitter header defaults to whatever
/// [`EmitterHeader::from_env`] resolves at writer-open time — set
/// `COSMON_EMITTER_KIND` / `COSMON_EMITTER_ID` / `COSMON_META_LEVEL`
/// in the environment to forward-fill a non-`Unknown` classification.
/// Call sites that need an explicit override should use
/// [`emit_one_with_emitter`].
///
/// # Errors
///
/// Forwards errors from [`EventLogWriter::open`] and [`EventLogWriter::emit`].
pub fn emit_one(
    events_path: impl Into<PathBuf>,
    event: EventV2,
    causal_parent: Option<Seq>,
) -> std::io::Result<Seq> {
    let mut writer = EventLogWriter::open(events_path)?;
    let seq = writer.emit(event, causal_parent)?;
    writer.sync()?;
    Ok(seq)
}

/// Convenience — open the writer, emit one event with an explicit
/// emitter header, sync, and drop.
///
/// Equivalent to [`emit_one`] but the emitter header is taken from
/// the supplied [`EmitterHeader`] rather than the
/// `COSMON_EMITTER_*` environment variables. Used by CLI commands
/// that know their classification at the call site (typical pattern:
/// `cs nucleate` passing `EmitterKind::Cli`,  `cs patrol` passing
/// `EmitterKind::Patrol`).
///
/// # Errors
///
/// Forwards errors from [`EventLogWriter::open`] and
/// [`EventLogWriter::emit_with_emitter`].
pub fn emit_one_with_emitter(
    events_path: impl Into<PathBuf>,
    event: EventV2,
    causal_parent: Option<Seq>,
    emitter: &EmitterHeader,
) -> std::io::Result<Seq> {
    let mut writer = EventLogWriter::open(events_path)?;
    let seq = writer.emit_with_emitter(event, causal_parent, emitter)?;
    writer.sync()?;
    Ok(seq)
}

/// Resolve the canonical events log path under a cosmon `state_dir`.
///
/// Encapsulates the literal `events.jsonl` filename so cross-crate
/// callers (notably `cosmon-rpp-adapter`, gated by the §8j-style
/// `no_state_read_test` static check) do not need to mention it
/// directly.
#[must_use]
pub fn resolve_events_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("events.jsonl")
}

/// Best-effort emit of an [`EventV2::InvocationCompleted`] event to
/// `<state_dir>/events.jsonl`.
///
/// Helper for `cosmon-rpp-adapter` (T-V1-IFBDD-METER): records that
/// one HTTP invocation through the Remote Pilot Port has just
/// completed. Defensive — a serialise or write failure is silently
/// swallowed in keeping with the seal pattern from
/// [`crate::briefing_seal`]. The hot path must never fail because
/// telemetry is unhappy.
///
/// `molecule_id` is `Some(s)` when the route targets a specific
/// molecule and `s` is a syntactically valid id; an unparseable id
/// degrades gracefully to `None` (the rejected request is still
/// recorded, just without the molecule reference).
pub fn emit_invocation_completed(
    state_dir: &Path,
    tenant: &str,
    molecule_id: Option<&str>,
    backend: &str,
    latency_ms: u64,
    success: bool,
) {
    let parsed_id = molecule_id.and_then(|id| cosmon_core::id::MoleculeId::new(id).ok());
    let event = EventV2::InvocationCompleted {
        tenant: tenant.to_owned(),
        molecule_id: parsed_id,
        backend: backend.to_owned(),
        latency_ms,
        success,
    };
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = emit_one(path, event, None);
}

/// Best-effort emit of an [`EventV2::FleetTyped`] event to
/// `<state_dir>/events.jsonl`.
///
/// Helper for the `cs fleet` command family: records that a fleet
/// with an advisory `organization_type` was loaded. Pure IFBDD
/// instrumentation — the value is **never** matched on. Defensive:
/// a serialise or write failure is silently swallowed (same trace-not-lock
/// discipline as [`emit_invocation_completed`] and [`crate::briefing_seal`]).
/// The hot path must never fail because telemetry is unhappy.
///
/// No-op when `organization_type` is `None` — the field is optional, and
/// fleets without a self-classification do not need to surface anything.
pub fn emit_fleet_typed(state_dir: &Path, fleet: &str, organization_type: Option<&str>) {
    let Some(organization_type) = organization_type else {
        return;
    };
    let event = EventV2::FleetTyped {
        fleet: fleet.to_owned(),
        organization_type: organization_type.to_owned(),
        ts: chrono::Utc::now(),
    };
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = emit_one(path, event, None);
}

/// Read every line of the log and yield (best-effort coerced) envelopes.
///
/// Lines that cannot be coerced at all are skipped — this function is for
/// replay and tooling, not for enforcing invariants.
///
/// # Errors
///
/// Returns `std::io::Error` if the file cannot be opened or read.
pub fn read_all(path: impl AsRef<Path>) -> std::io::Result<Vec<Envelope>> {
    let file = File::open(path.as_ref())?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(env) = Envelope::from_line(&line) {
            out.push(env);
        }
    }
    Ok(out)
}

/// Acquire `flock(LOCK_EX)` on `file`, fast-path non-blocking, fall through
/// to a blocking wait if the lock is contended.
///
/// The fast path is `try_lock_exclusive` — a single non-blocking syscall.
/// When it returns `WouldBlock` we briefly spin (sub-second polling at
/// [`LOCK_RETRY_INTERVAL`] for up to [`LOCK_FAST_BUDGET`]) so a momentary
/// collision does not cost us a context switch on the queued path. If we
/// are still blocked after the spin budget we fall back to blocking
/// `lock_exclusive` — the kernel queues waiters and serves them in
/// arrival order, which is the only way to avoid lockstep starvation
/// under sustained N-way contention.
///
/// "Never block indefinitely" (ADR-052 §I7) is preserved by every writer
/// holding the lock for *O(line bytes)* and releasing immediately — the
/// blocking fallback drains as fast as the slowest `write_all` + fsync. A
/// stuck holder is a separate failure mode and is the responsibility of
/// out-of-band liveness sweeps (`cs patrol`).
///
/// The caller is responsible for calling `FileExt::unlock` after the
/// protected critical section.
fn acquire_lock(file: &File) -> std::io::Result<()> {
    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(e) if would_block(&e) => {
                if start.elapsed() >= LOCK_FAST_BUDGET {
                    // Fast-spin window exhausted — let the kernel queue us.
                    return file.lock_exclusive();
                }
                std::thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Resolve the Kahneman K1 [`QualityBand`] for a verb emission.
///
/// `now` is the wall-clock instant the writer is about to stamp; the local
/// hour is derived against the host timezone (operator-local). `last_verb`
/// is the timestamp of the most recently observed verb in this writer's
/// view of the log (`None` on cold start). Pure helper — composes the
/// signals into the input shape [`quality_band::compute`] expects.
fn compute_band_for(now: DateTime<Utc>, last_verb: Option<DateTime<Utc>>) -> QualityBand {
    let local_hour = quality_band::local_hour_of(now);
    let latency_s = last_verb.and_then(|prev| {
        // The wall clock is non-decreasing in practice but not guaranteed;
        // a backwards step (NTP slew, manual override) yields zero rather
        // than a wrap-around.
        let delta = now.signed_duration_since(prev);
        if delta.num_seconds() < 0 {
            None
        } else {
            u64::try_from(delta.num_seconds()).ok()
        }
    });
    quality_band::compute(local_hour, latency_s)
}

fn would_block(e: &std::io::Error) -> bool {
    // fs2 reports lock contention as `WouldBlock` on every platform we
    // currently support. Treat `Other` defensively in case a future
    // implementation surfaces `EAGAIN` differently.
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Other
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::MoleculeId;
    use tempfile::tempdir;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    #[test]
    fn writer_assigns_monotone_sequence_from_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        assert_eq!(w.next_seq(), Seq(0));

        let s0 = w
            .emit(
                EventV2::MoleculeNucleated {
                    molecule_id: mid("cs-20260411-aaaa"),
                    formula_id: "f".to_owned(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
                None,
            )
            .unwrap();
        let s1 = w
            .emit(
                EventV2::MoleculeCompleted {
                    molecule_id: mid("cs-20260411-aaaa"),
                    duration_ms: Some(500),
                    reason: "ok".to_owned(),
                },
                Some(s0),
            )
            .unwrap();
        assert_eq!(s0, Seq(0));
        assert_eq!(s1, Seq(1));
        w.sync().unwrap();
    }

    #[test]
    fn writer_resumes_sequence_across_opens() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        {
            let mut w = EventLogWriter::open(&path).unwrap();
            w.emit(
                EventV2::MoleculeNucleated {
                    molecule_id: mid("cs-20260411-aaaa"),
                    formula_id: "f".to_owned(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
                None,
            )
            .unwrap();
            w.emit(
                EventV2::MoleculeNucleated {
                    molecule_id: mid("cs-20260411-bbbb"),
                    formula_id: "f".to_owned(),
                    parent_id: None,
                    blocks: Vec::new(),
                },
                None,
            )
            .unwrap();
            w.sync().unwrap();
        }

        let mut w2 = EventLogWriter::open(&path).unwrap();
        assert_eq!(w2.next_seq(), Seq(2));
        let s = w2
            .emit(
                EventV2::MoleculeCompleted {
                    molecule_id: mid("cs-20260411-aaaa"),
                    duration_ms: None,
                    reason: "done".to_owned(),
                },
                None,
            )
            .unwrap();
        assert_eq!(s, Seq(2));
    }

    #[test]
    fn read_all_returns_envelopes_in_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        w.emit(
            EventV2::MoleculeNucleated {
                molecule_id: mid("cs-20260411-aaaa"),
                formula_id: "f".to_owned(),
                parent_id: None,
                blocks: Vec::new(),
            },
            None,
        )
        .unwrap();
        w.emit(
            EventV2::MoleculeCompleted {
                molecule_id: mid("cs-20260411-aaaa"),
                duration_ms: Some(1),
                reason: "ok".to_owned(),
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();

        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].seq, Seq(0));
        assert_eq!(envs[1].seq, Seq(1));
    }

    #[test]
    fn read_all_tolerates_legacy_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"molecule_nucleated","molecule_id":"cs-20260411-aaaa","formula_id":"task-work","timestamp":"2026-04-11T10:00:00Z"}"#,
                "\n",
                r#"{"kind":"worker_terminated","worker_id":"quartz","reason":"timeout","timestamp":"2026-04-11T10:01:00Z"}"#,
                "\n",
                r#"{"foo":"bar"}"#,
                "\n",
            ),
        ).unwrap();

        let envs = read_all(&path).unwrap();
        assert_eq!(
            envs.len(),
            2,
            "two legacy lines should coerce, one rejected"
        );
    }

    #[test]
    fn emit_assigns_per_molecule_sequence_independently() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();

        let a = mid("cs-20260411-aaaa");
        let b = mid("cs-20260411-bbbb");

        // Interleave events for two molecules: a, b, a, a, b.
        w.emit(
            EventV2::MoleculeNucleated {
                molecule_id: a.clone(),
                formula_id: "f".into(),
                parent_id: None,
                blocks: vec![],
            },
            None,
        )
        .unwrap();
        w.emit(
            EventV2::MoleculeNucleated {
                molecule_id: b.clone(),
                formula_id: "f".into(),
                parent_id: None,
                blocks: vec![],
            },
            None,
        )
        .unwrap();
        w.emit(
            EventV2::MoleculeStepCompleted {
                molecule_id: a.clone(),
                step: 0,
                total: 2,
                duration_ms: None,
                step_hash: None,
            },
            None,
        )
        .unwrap();
        w.emit(
            EventV2::MoleculeCompleted {
                molecule_id: a.clone(),
                duration_ms: None,
                reason: "ok".into(),
            },
            None,
        )
        .unwrap();
        w.emit(
            EventV2::MoleculeCompleted {
                molecule_id: b.clone(),
                duration_ms: None,
                reason: "ok".into(),
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();

        let envs = read_all(&path).unwrap();
        let mol_seqs_a: Vec<_> = envs
            .iter()
            .filter(|e| e.event.molecule_id() == Some(&a))
            .map(|e| e.mol_seq.unwrap().0)
            .collect();
        let mol_seqs_b: Vec<_> = envs
            .iter()
            .filter(|e| e.event.molecule_id() == Some(&b))
            .map(|e| e.mol_seq.unwrap().0)
            .collect();
        assert_eq!(mol_seqs_a, vec![0, 1, 2], "molecule a sequence");
        assert_eq!(mol_seqs_b, vec![0, 1], "molecule b sequence");
    }

    #[test]
    fn worker_only_event_has_no_mol_seq() {
        use cosmon_core::id::WorkerId;
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        w.emit(
            EventV2::WorkerKilled {
                worker_id: WorkerId::new("quartz").unwrap(),
                reason: "purge".into(),
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();
        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert!(
            envs[0].mol_seq.is_none(),
            "worker-only event has no mol_seq"
        );
    }

    #[test]
    fn second_writer_observes_first_under_lock() {
        // Two writers in the same process race for the same file. flock
        // serialises them; the late writer catches up to the tail and
        // assigns a fresh global seq instead of clobbering.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = mid("cs-20260411-aaaa");

        let mut a = EventLogWriter::open(&path).unwrap();
        let mut b = EventLogWriter::open(&path).unwrap();

        let s_a = a
            .emit(
                EventV2::MoleculeNucleated {
                    molecule_id: m.clone(),
                    formula_id: "f".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                None,
            )
            .unwrap();
        assert_eq!(s_a, Seq(0));
        a.sync().unwrap();

        let s_b = b
            .emit(
                EventV2::MoleculeStepCompleted {
                    molecule_id: m.clone(),
                    step: 0,
                    total: 1,
                    duration_ms: None,
                    step_hash: None,
                },
                None,
            )
            .unwrap();
        assert_eq!(
            s_b,
            Seq(1),
            "second writer must observe the first under lock"
        );
        b.sync().unwrap();

        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].seq, Seq(0));
        assert_eq!(envs[1].seq, Seq(1));
        assert_eq!(envs[0].mol_seq, Some(Seq(0)));
        assert_eq!(envs[1].mol_seq, Some(Seq(1)));
    }

    // -- Kahneman K1 — quality_band stamping (delib-20260503-9aab TQ4) --

    #[test]
    fn verb_emission_carries_quality_band() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        w.emit(
            EventV2::MoleculeNucleated {
                molecule_id: mid("cs-20260503-aaaa"),
                formula_id: "f".into(),
                parent_id: None,
                blocks: vec![],
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();
        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert!(
            envs[0].quality_band.is_some(),
            "verb emission must carry a K1 quality_band"
        );
    }

    #[test]
    fn non_verb_event_has_no_quality_band() {
        use cosmon_core::id::WorkerId;
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        w.emit(
            EventV2::WorkerHeartbeat {
                worker_id: WorkerId::new("quartz").unwrap(),
                ts: chrono::Utc::now(),
                activity_hint: cosmon_core::event_v2::ActivityHint::Unknown,
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();
        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert!(
            envs[0].quality_band.is_none(),
            "non-verb event must NOT carry a quality_band"
        );
    }

    #[test]
    fn band_helper_resolves_diurne_at_noon() {
        use chrono::TimeZone;
        // Pick a UTC time the operator's local timezone interprets as
        // mid-day (offset agnostic — local hour 12 will hold for any
        // zone where the offset is small ; the helper is meant to be
        // pure on its inputs in `quality_band::compute`, so this test
        // simply exercises the wiring rather than the timezone math).
        let now = chrono::Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap();
        // No prior verb -> latency is None.
        let band = compute_band_for(now, None);
        // Hour 12 is unambiguously diurne in every conceivable host
        // timezone where offset ∈ (-7, +9) — broad enough for the test
        // to be deterministic on the operator's machine.
        let local_hour = quality_band::local_hour_of(now);
        if (10..23).contains(&local_hour) {
            assert_eq!(band, QualityBand::Diurne, "local hour {local_hour}");
        }
    }

    // Emitter header tests (cosmon-ward §F1, task-20260509-7210).

    #[test]
    fn emit_forward_fills_default_unknown_emitter_when_writer_unset() {
        // Write through `set_emitter(default)` to bypass any env vars
        // the test environment may have inherited from the runner.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        let mut w = EventLogWriter::open(&path).unwrap();
        w.set_emitter(EmitterHeader::default());
        assert_eq!(w.emitter().kind, EmitterKind::Unknown);
        w.emit(
            EventV2::WorkerHeartbeat {
                worker_id: cosmon_core::id::WorkerId::new("quartz").unwrap(),
                ts: Utc::now(),
                activity_hint: cosmon_core::event_v2::ActivityHint::Idle,
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();
        drop(w);

        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].emitter_kind, EmitterKind::Unknown);
        assert_eq!(envs[0].emitter_id, "");
        assert_eq!(envs[0].meta_level, 0);
    }

    #[test]
    fn emit_with_emitter_overrides_sticky_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut w = EventLogWriter::open(&path).unwrap();
        w.set_emitter(EmitterHeader::new(EmitterKind::Cli, "cli:cs-test", 0));
        // First event uses sticky.
        w.emit(
            EventV2::MoleculeNucleated {
                molecule_id: mid("cs-20260509-aaaa"),
                formula_id: "f".into(),
                parent_id: None,
                blocks: vec![],
            },
            None,
        )
        .unwrap();
        // Second event overrides without disturbing sticky.
        let attendant = EmitterHeader::new(EmitterKind::Attendant, "attendant:silence-watch", 1);
        w.emit_with_emitter(
            EventV2::MoleculeStatusChanged {
                molecule_id: mid("cs-20260509-aaaa"),
                from: "running".into(),
                to: "stuck".into(),
            },
            None,
            &attendant,
        )
        .unwrap();
        // Third event reverts to sticky.
        w.emit(
            EventV2::MoleculeCompleted {
                molecule_id: mid("cs-20260509-aaaa"),
                duration_ms: Some(123),
                reason: "ok".into(),
            },
            None,
        )
        .unwrap();
        w.sync().unwrap();

        let envs = read_all(&path).unwrap();
        assert_eq!(envs[0].emitter_kind, EmitterKind::Cli);
        assert_eq!(envs[0].emitter_id, "cli:cs-test");
        assert_eq!(envs[0].meta_level, 0);
        assert_eq!(envs[1].emitter_kind, EmitterKind::Attendant);
        assert_eq!(envs[1].emitter_id, "attendant:silence-watch");
        assert_eq!(envs[1].meta_level, 1);
        assert_eq!(envs[2].emitter_kind, EmitterKind::Cli);
        assert_eq!(envs[2].emitter_id, "cli:cs-test");
    }

    #[test]
    fn emit_one_with_emitter_writes_explicit_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let emitter = EmitterHeader::new(EmitterKind::Patrol, "patrol:silence", 0);
        emit_one_with_emitter(
            &path,
            EventV2::WorkerSilenceDetected {
                molecule_id: mid("cs-20260509-aaaa"),
                worker_id: None,
                age_since_last_heartbeat_s: Some(240),
                threshold_s: 90,
            },
            None,
            &emitter,
        )
        .unwrap();

        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].emitter_kind, EmitterKind::Patrol);
        assert_eq!(envs[0].emitter_id, "patrol:silence");
    }

    #[test]
    fn legacy_envelope_without_emitter_is_readable() {
        // Manually drop a pre-F1 line into the file, then read_all.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            "{\"seq\":0,\"timestamp\":\"2026-04-11T10:00:00Z\",\"type\":\"molecule_nucleated\",\"molecule_id\":\"cs-20260411-aaaa\",\"formula_id\":\"task-work\"}\n",
        )
        .unwrap();
        let envs = read_all(&path).unwrap();
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].emitter_kind, EmitterKind::Unknown);
        assert_eq!(envs[0].emitter_id, "");
        assert_eq!(envs[0].meta_level, 0);
    }

    #[test]
    fn last_verb_ts_persists_across_writer_reopens() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let m = mid("cs-20260503-bbbb");
        {
            let mut w = EventLogWriter::open(&path).unwrap();
            w.emit(
                EventV2::MoleculeNucleated {
                    molecule_id: m.clone(),
                    formula_id: "f".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                None,
            )
            .unwrap();
            w.sync().unwrap();
            assert!(w.last_verb_ts.is_some());
        }
        // Reopen — the catch-up scan should re-populate `last_verb_ts`
        // from the persisted log so the K1 latency signal survives
        // process restarts.
        let w = EventLogWriter::open(&path).unwrap();
        assert!(
            w.last_verb_ts.is_some(),
            "writer must recover last_verb_ts from disk on reopen"
        );
    }
}
