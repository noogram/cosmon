// SPDX-License-Identifier: AGPL-3.0-only

//! Preserve edits before replacing the generated briefing on a step change.
//!
//! The raw digest is separate from the canonical-text verification seals:
//! even an editor's newline conversion must be preserved byte for byte.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const RENDER_DIGEST: &str = "briefing.md.rendered-blake3";

/// Remember the bytes cosmon rendered, after all generated additions landed.
///
/// # Errors
///
/// Returns an I/O error if the briefing cannot be read or its digest saved.
pub fn record_rendered_briefing(mol_dir: &Path) -> io::Result<()> {
    let bytes = fs::read(mol_dir.join("briefing.md"))?;
    // Atomic replacement also supports a read-only previous digest: the
    // directory owns replacement, rather than the old file's write bit.
    crate::work_message::write_atomic(
        &mol_dir.join(RENDER_DIGEST),
        cosmon_hash::Hash::of_bytes(&bytes).to_hex().as_bytes(),
    )
}

/// Keep a changed briefing under its one-based step number before rewriting it.
///
/// A missing render record (legacy molecule) conservatively keeps the file.
/// An existing identical backup is reused without writing it, making retries
/// safe. A different backup refuses the rewrite; neither edit may be lost.
///
/// # Errors
///
/// Returns an I/O error if the briefing or digest cannot be read, the backup
/// cannot be saved, or its name already holds different bytes.
pub fn preserve_briefing_edit(mol_dir: &Path, step: usize) -> io::Result<Option<PathBuf>> {
    let current = match fs::read(mol_dir.join("briefing.md")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if matches_render(mol_dir, &current)? {
        return Ok(None);
    }

    let backup = mol_dir.join(format!("briefing.md.edited-{step}"));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
    {
        Ok(mut file) => {
            file.write_all(&current)?;
            file.sync_all()?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&backup)?.file_type().is_file()
                || fs::read(&backup)? != current
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} is not an identical regular backup; move that backup to a new path, then retry (briefing.md is unchanged)",
                        backup.display()
                    ),
                ));
            }
        }
        Err(e) => return Err(e),
    }
    Ok(Some(backup))
}

/// Refresh the render digest after a generated addition, retaining prior edits.
///
/// `previous` is the content before cosmon appended its generated text. Only
/// an unedited input may acquire a new baseline; otherwise the next advance
/// must still preserve the hand-edited content and the generated addition.
///
/// # Errors
///
/// Returns an I/O error if the render record cannot be read or updated.
pub fn record_briefing_addition(mol_dir: &Path, previous: &[u8]) -> io::Result<()> {
    if matches_render(mol_dir, previous)? {
        record_rendered_briefing(mol_dir)?;
    }
    Ok(())
}

fn matches_render(mol_dir: &Path, bytes: &[u8]) -> io::Result<bool> {
    match fs::read_to_string(mol_dir.join(RENDER_DIGEST)) {
        Ok(digest) => Ok(digest == cosmon_hash::Hash::of_bytes(bytes).to_hex()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Replace a generated briefing without truncating a hard-linked saved copy.
///
/// Step rendering and generated additions use a fresh file, so an existing
/// backup that aliases the old briefing retains its bytes and modification time.
///
/// # Errors
///
/// Returns an I/O error if the replacement cannot be written and installed.
pub fn write_generated_briefing(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    crate::work_message::write_atomic(path, contents.as_ref())
}
