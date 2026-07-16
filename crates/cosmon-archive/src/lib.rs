// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical archive writer for terminal Cosmon molecules (ADR-030, M2).
//!
//! When a molecule reaches a terminal transition (`cs done`, `cs collapse`,
//! `cs freeze`, `cs stuck`) and the project has `[archive].enabled = true`
//! in `.cosmon/config.toml`, the worker's durable artifacts are copied from
//! the live molecule directory into a tracked, append-only snapshot under
//! `.cosmon/archive/<YYYY>/<MM>/<molecule_id>/`.
//!
//! The archive outlives worktree teardown and branch deletion: a fresh
//! clone of the project sees the full chain of reasoning behind every
//! merged molecule without having to re-run `cs`.
//!
//! # Layout
//!
//! ```text
//! .cosmon/archive/
//!   SCHEMA_VERSION                       # monotonically-increasing integer
//!   <YYYY>/<MM>/<molecule_id>/
//!     prompt.md                          # nucleation payload
//!     briefing.md                        # rendered formula plan
//!     synthesis.md                       # deliberation synthesis (if any)
//!     responses/                         # panel responses (if any)
//!       <persona>.md ...
//!     log.md                             # append-only worker log
//!     events.jsonl                       # per-molecule transition log
//!     state.json                         # terminal state snapshot
//!     manifest.json                      # SCHEMA_VERSION + hash chain
//! ```
//!
//! # Atomicity
//!
//! Every file is written with the tempfile + rename dance — the destination
//! only exists once its bytes have been fully flushed and `fsync`ed.
//! A crash between files leaves the entry in a partially-populated state;
//! re-running `archive::write` is idempotent for content (hashes are
//! deterministic) and monotonic for the fleet-level `events` stream.
//!
//! # Hash chain
//!
//! The archive's integrity story is a linear [BLAKE3] chain over the files
//! listed in the manifest, in lexicographic order by archived name:
//!
//! ```text
//! prev_0 = H("") = BLAKE3 of empty bytes
//! file_i.prev_hash   = prev_{i-1}
//! file_i.content_hash = BLAKE3(file_bytes)
//! file_i.chain_hash   = BLAKE3(prev_i || name_i || content_hash_i)
//! chain_head          = file_N.chain_hash
//! ```
//!
//! The chain head travels in `manifest.chain_head`. Tampering with any
//! archived file changes `content_hash`, which cascades to every
//! `chain_hash` after it and ultimately to `chain_head`. Callers
//! (`cs verify`, auditors, future witness tooling) only need to compare
//! a recomputed chain head against the stored one to detect silent
//! post-hoc edits.
//!
//! Content hashing is delegated to [`cosmon_hash::Hash`] so the algorithm
//! choice is centralized — swapping BLAKE3 for a keyed MAC later will
//! not ripple through every archive file.
//!
//! [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// Archive schema version.
///
/// Bumped any time the on-disk layout changes in a way a consumer would
/// care about (file added/removed, manifest shape changed). Read by
/// `cs verify` to pick the right parser.
pub const SCHEMA_VERSION: u32 = 1;

/// The kind of terminal transition that produced an archive entry.
///
/// Recorded in `manifest.json` so consumers can filter (e.g. "show me
/// every `collapse` from 2026-04") without re-reading every molecule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// `cs done` — molecule merged back to the main branch.
    Done,
    /// `cs collapse` — terminated with a recorded reason.
    Collapse,
    /// `cs freeze` — worker paused; durable state captured so
    /// `cs thaw` (or a reclone) can resume from the freeze point.
    Freeze,
    /// `cs stuck` — worker blocked on a missing prerequisite.
    Stuck,
}

impl Trigger {
    /// Lowercase string tag used in manifest / events.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Collapse => "collapse",
            Self::Freeze => "freeze",
            Self::Stuck => "stuck",
        }
    }
}

/// Error returned when the canonical archive write fails partway through.
///
/// Archive writes are intentionally best-effort at the call site —
/// [`write_non_fatal`] converts this to a warning on stderr.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// Filesystem I/O failed (create/read/write/rename).
    #[error("archive I/O error at {path}: {source}")]
    Io {
        /// Path where the error occurred.
        path: PathBuf,
        /// Underlying std I/O error.
        #[source]
        source: io::Error,
    },
    /// Serializing the manifest to canonical JSON failed.
    #[error("archive manifest serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl ArchiveError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// One file's entry in the manifest's hash chain.
///
/// `name` is the file's path relative to the molecule entry directory
/// (e.g. `"responses/torvalds.md"`), always using `/` separators so the
/// manifest is portable across filesystems.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path inside the molecule entry directory. Uses `/`.
    pub name: String,
    /// Size in bytes of the archived content.
    pub size: u64,
    /// BLAKE3 hash of the file's raw bytes.
    pub content_hash: Hash,
    /// Chain hash of the previous entry (hex). Genesis value is the
    /// BLAKE3 of the empty byte string.
    pub prev_hash: Hash,
    /// Chain hash of this entry: `BLAKE3(prev_hash || name || content_hash)`.
    pub chain_hash: Hash,
}

/// The archive manifest written to `manifest.json`.
///
/// This is the single source of truth for "what is in this archive entry
/// and is it still intact?". `cs verify` rebuilds the chain from the
/// files on disk and compares the recomputed `chain_head` against the
/// stored value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Molecule identifier (e.g. `task-20260413-dfd8`).
    pub molecule_id: String,
    /// Terminal transition kind.
    pub trigger: Trigger,
    /// UTC timestamp at which the archive was written.
    pub archived_at: DateTime<Utc>,
    /// Ordered list of files in the chain.
    pub files: Vec<FileEntry>,
    /// Last `chain_hash` in `files`. Defaults to the genesis hash when
    /// `files` is empty.
    pub chain_head: Hash,
}

/// Outcome of a successful archive write.
#[derive(Debug)]
pub struct ArchiveWrite {
    /// Directory that received the archive (`<archive_root>/YYYY/MM/<id>/`).
    pub entry_dir: PathBuf,
    /// The manifest that was written.
    pub manifest: Manifest,
}

/// Snapshot a live molecule directory into the archive.
///
/// `archive_root` is `.cosmon/archive/`. `mol_dir` is the directory
/// whose artifacts (`prompt.md`, `briefing.md`, …) should be captured.
/// `molecule_id` is used to route the write into `YYYY/MM/<id>/`.
///
/// The set of files searched for in `mol_dir` is fixed (see the module
/// docstring). Missing files are silently skipped so the writer works
/// on partial molecule directories — the manifest only lists what was
/// actually archived. The `responses/` directory is recursed one level
/// deep; nested subdirectories are not archived.
///
/// Every destination file is written atomically (write-to-`.tmp` +
/// `fsync` + `rename`). The manifest is written *last*: if it is
/// present on disk, the chain is trustworthy up to a readable prev/next
/// comparison. If it is missing, the entry is considered unfinished.
///
/// # Errors
///
/// Returns [`ArchiveError`] if any filesystem or serialization step
/// fails. The caller is responsible for deciding whether this is
/// fatal (test suites, `cs verify --strict`) or a warning
/// (`cs done`, `cs collapse` — see [`write_non_fatal`]).
pub fn write(
    archive_root: &Path,
    mol_dir: &Path,
    molecule_id: &str,
    now: DateTime<Utc>,
    trigger: Trigger,
) -> Result<ArchiveWrite, ArchiveError> {
    let month_dir = archive_root
        .join(format!("{:04}", now.year()))
        .join(format!("{:02}", now.month()));
    let entry_dir = month_dir.join(molecule_id);
    fs::create_dir_all(&entry_dir).map_err(|e| ArchiveError::io(&entry_dir, e))?;

    write_schema_version(archive_root)?;

    // Discover which of the canonical artifacts exist in the live
    // molecule directory. Order matters for human readability of the
    // manifest, but the chain is rebuilt in lexicographic order below,
    // so this list is not security-critical.
    let mut planned: Vec<(String, PathBuf)> = Vec::new();

    for name in [
        "prompt.md",
        "briefing.md",
        "synthesis.md",
        "log.md",
        "events.jsonl",
        "state.json",
    ] {
        let src = mol_dir.join(name);
        if src.is_file() {
            planned.push((name.to_owned(), src));
        }
    }

    // `responses/` — one-level-deep subtree.
    let responses_src = mol_dir.join("responses");
    if responses_src.is_dir() {
        let mut children: Vec<_> = fs::read_dir(&responses_src)
            .map_err(|e| ArchiveError::io(&responses_src, e))?
            .filter_map(Result::ok)
            .collect();
        // Stable order so the plan is reproducible.
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let meta = child
                .file_type()
                .map_err(|e| ArchiveError::io(child.path(), e))?;
            if meta.is_file() {
                let name = format!("responses/{}", child.file_name().to_string_lossy());
                planned.push((name, child.path()));
            }
        }
    }

    // Copy each file atomically and collect its content bytes for the
    // chain.
    let mut content_hashes: Vec<(String, u64, Hash)> = Vec::with_capacity(planned.len());
    for (name, src) in &planned {
        let bytes = fs::read(src).map_err(|e| ArchiveError::io(src, e))?;
        let dest = entry_dir.join(name);
        write_atomic(&dest, &bytes)?;
        let content_hash = Hash::of_bytes(&bytes);
        let size = bytes.len() as u64;
        content_hashes.push((name.clone(), size, content_hash));
    }

    // Build the hash chain in lexicographic order over archived file
    // names. Lexicographic sort is important so the chain is
    // reconstructible by any consumer walking the entry directory:
    // they don't need to know the original `planned` order.
    content_hashes.sort_by(|a, b| a.0.cmp(&b.0));

    let genesis = Hash::of_bytes(&[]);
    let mut prev = genesis;
    let mut files: Vec<FileEntry> = Vec::with_capacity(content_hashes.len());
    for (name, size, content_hash) in content_hashes {
        let chain_hash = link_hash(&prev, &name, &content_hash);
        files.push(FileEntry {
            name,
            size,
            content_hash,
            prev_hash: prev,
            chain_hash,
        });
        prev = chain_hash;
    }

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        molecule_id: molecule_id.to_owned(),
        trigger,
        archived_at: now,
        files,
        chain_head: prev,
    };

    // Manifest is written last and atomically. Its presence signals a
    // complete, readable archive entry.
    let manifest_path = entry_dir.join("manifest.json");
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let mut manifest_bytes = manifest_bytes;
    manifest_bytes.push(b'\n');
    write_atomic(&manifest_path, &manifest_bytes)?;

    Ok(ArchiveWrite {
        entry_dir,
        manifest,
    })
}

/// Best-effort variant for trigger sites that must never fail.
///
/// Used by `cs done`, `cs collapse`, `cs freeze`, `cs stuck`: the
/// terminal transition is the primary operation, archiving is a
/// side-effect that improves observability. A failure to archive is
/// printed to stderr (prefixed with `archive:`) and swallowed.
#[must_use]
pub fn write_non_fatal(
    archive_root: &Path,
    mol_dir: &Path,
    molecule_id: &str,
    now: DateTime<Utc>,
    trigger: Trigger,
) -> Option<ArchiveWrite> {
    match write(archive_root, mol_dir, molecule_id, now, trigger) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!(
                "archive: failed to write {} entry for {molecule_id}: {e}",
                trigger.as_str(),
            );
            None
        }
    }
}

/// Recompute the chain head for a known-good sequence of archived
/// files. Exposed so `cs verify` and integration tests can share the
/// exact recipe the writer uses.
#[must_use]
pub fn recompute_chain_head(files: &[FileEntry]) -> Hash {
    let mut prev = Hash::of_bytes(&[]);
    for f in files {
        prev = link_hash(&prev, &f.name, &f.content_hash);
    }
    prev
}

fn link_hash(prev: &Hash, name: &str, content: &Hash) -> Hash {
    // Serialize each field with an explicit length prefix so that
    // e.g. names `"a/b"` + `"c"` can't be confused with `"a"` + `"/bc"`.
    // This is the classic length-tagged canonical concatenation used
    // by git, IPFS, and every hash-chain design that survives review.
    let mut buf = Vec::with_capacity(32 + name.len() + 32 + 24);
    buf.extend_from_slice(prev.as_bytes());
    buf.extend_from_slice(&(name.len() as u64).to_le_bytes());
    buf.extend_from_slice(name.as_bytes());
    buf.extend_from_slice(content.as_bytes());
    Hash::of_bytes(&buf)
}

fn write_schema_version(archive_root: &Path) -> Result<(), ArchiveError> {
    fs::create_dir_all(archive_root).map_err(|e| ArchiveError::io(archive_root, e))?;
    let path = archive_root.join("SCHEMA_VERSION");
    write_atomic(&path, format!("{SCHEMA_VERSION}\n").as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ArchiveError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ArchiveError::io(parent, e))?;
    }
    // Use a sibling tempfile so rename stays within the same
    // filesystem (atomic on POSIX).
    let tmp = path.with_extension("archive.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| ArchiveError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| ArchiveError::io(&tmp, e))?;
        f.sync_all().map_err(|e| ArchiveError::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| ArchiveError::io(path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genesis_hash() -> Hash {
        Hash::of_bytes(&[])
    }

    #[test]
    fn trigger_round_trips_through_serde() {
        for t in [
            Trigger::Done,
            Trigger::Collapse,
            Trigger::Freeze,
            Trigger::Stuck,
        ] {
            let s = serde_json::to_string(&t).unwrap();
            let back: Trigger = serde_json::from_str(&s).unwrap();
            assert_eq!(back, t);
        }
    }

    #[test]
    fn trigger_as_str_matches_serde() {
        let s = serde_json::to_string(&Trigger::Done).unwrap();
        assert_eq!(s, "\"done\"");
        assert_eq!(Trigger::Done.as_str(), "done");
    }

    #[test]
    fn empty_molecule_dir_produces_empty_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("archive");
        let mol = tmp.path().join("mol-empty");
        fs::create_dir_all(&mol).unwrap();

        let when = Utc::now();
        let w = write(&archive, &mol, "task-empty", when, Trigger::Done).unwrap();
        assert!(w.manifest.files.is_empty());
        assert_eq!(w.manifest.chain_head, genesis_hash());
    }

    #[test]
    fn link_hash_is_position_sensitive() {
        let prev = Hash::of_bytes(b"alpha");
        let content = Hash::of_bytes(b"beta");
        // Same prev, same content, different name → different link.
        let link_foo = link_hash(&prev, "foo", &content);
        let link_bar = link_hash(&prev, "bar", &content);
        assert_ne!(link_foo, link_bar);
        // Same prev, same name, different content → different link.
        let other = link_hash(&prev, "foo", &Hash::of_bytes(b"gamma"));
        assert_ne!(link_foo, other);
    }

    #[test]
    fn recompute_chain_head_matches_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("archive");
        let mol = tmp.path().join("mol");
        fs::create_dir_all(&mol).unwrap();
        fs::write(mol.join("prompt.md"), "prompt\n").unwrap();
        fs::write(mol.join("state.json"), "{}\n").unwrap();

        let when = Utc::now();
        let w = write(&archive, &mol, "task-xyz", when, Trigger::Done).unwrap();

        let recomputed = recompute_chain_head(&w.manifest.files);
        assert_eq!(recomputed, w.manifest.chain_head);
    }
}
