// SPDX-License-Identifier: AGPL-3.0-only

//! `cs note <id> "text"` — append an audit-trail note to a molecule.
//!
//! Notes are append-only: once written, the file must never be edited
//! or deleted. Each note lands in the molecule directory under
//! `notes/NNN-author.md` where `NNN` is a zero-padded monotonic index
//! and `author` is either the worker id (with `--as-worker`) or `human`.

use std::fs;
use std::io::Read;
use std::path::Path;

use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::note::{HumanMarker, Note, NoteAuthor};

use super::Context;

/// Arguments for the `note` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to annotate.
    pub molecule_id: String,

    /// Note body (positional). Omit with `--edit` to open `$EDITOR`.
    pub body: Option<String>,

    /// Open `$EDITOR` to compose the note instead of providing inline text.
    #[arg(long)]
    pub edit: bool,

    /// Author the note as a worker (records the worker id in frontmatter).
    ///
    /// Without this flag, the note is attributed to `human`.
    #[arg(long = "as-worker", value_name = "WORKER_ID")]
    pub as_worker: Option<String>,
}

/// Execute the `note` command.
///
/// # Errors
/// Fails if the molecule does not exist, no body was supplied, or
/// filesystem operations fail.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let mol_id =
        MoleculeId::new(&args.molecule_id).map_err(|e| anyhow::anyhow!("invalid id: {e}"))?;

    // Verify the molecule exists before writing anything.
    store
        .load_molecule(&mol_id)
        .map_err(|e| anyhow::anyhow!("failed to load molecule: {e}"))?;

    let body = resolve_body(args)?;
    if body.trim().is_empty() {
        return Err(anyhow::anyhow!("note body is empty"));
    }

    let author = match &args.as_worker {
        Some(w) => NoteAuthor::Worker(
            WorkerId::new(w).map_err(|e| anyhow::anyhow!("invalid worker id: {e}"))?,
        ),
        None => NoteAuthor::Human(HumanMarker),
    };

    let mol_dir = store.molecule_dir(&mol_id);
    let notes_dir = mol_dir.join("notes");
    fs::create_dir_all(&notes_dir)
        .map_err(|e| anyhow::anyhow!("failed to create notes dir: {e}"))?;

    let seq = next_seq(&notes_dir);
    let note = Note::new(seq, author, body);
    let path = notes_dir.join(note.file_name());
    fs::write(&path, note.render()).map_err(|e| anyhow::anyhow!("failed to write note: {e}"))?;

    if ctx.json {
        let out = serde_json::json!({
            "id": mol_id.as_str(),
            "seq": note.seq,
            "author": note.author.to_string(),
            "timestamp": note.timestamp.to_rfc3339(),
            "path": path.to_string_lossy(),
        });
        println!("{out}");
    } else {
        println!("note {} written to {}", note.seq, path.display());
    }
    Ok(())
}

/// Choose between the inline body, `$EDITOR`, or an error if both or neither.
fn resolve_body(args: &Args) -> anyhow::Result<String> {
    match (&args.body, args.edit) {
        (Some(_), true) => Err(anyhow::anyhow!(
            "pass either a positional body or --edit, not both"
        )),
        (Some(b), false) => Ok(b.clone()),
        (None, true) => open_editor(),
        (None, false) => Err(anyhow::anyhow!("missing note body (or use --edit)")),
    }
}

/// Spawn `$EDITOR` on a tempfile and return the trimmed contents.
fn open_editor() -> anyhow::Result<String> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
    let tmp = tempfile::Builder::new()
        .prefix("cs-note-")
        .suffix(".md")
        .tempfile()
        .map_err(|e| anyhow::anyhow!("failed to create tempfile: {e}"))?;
    let path = tmp.path().to_path_buf();
    drop(tmp); // release the handle so the editor can write

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn editor `{editor}`: {e}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!("editor exited with status {status}"));
    }
    let mut buf = String::new();
    fs::File::open(&path)
        .and_then(|mut f| f.read_to_string(&mut buf))
        .map_err(|e| anyhow::anyhow!("failed to read editor output: {e}"))?;
    let _ = fs::remove_file(&path);
    Ok(buf)
}

/// Compute the next monotonic sequence number by scanning `notes/` for
/// the highest existing `NNN-...md` prefix.
pub(crate) fn next_seq(notes_dir: &Path) -> u32 {
    let mut max = 0u32;
    let Ok(entries) = fs::read_dir(notes_dir) else {
        return 1;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !has_md_ext(name) {
            continue;
        }
        let Some((prefix, _rest)) = name.split_once('-') else {
            continue;
        };
        if let Ok(n) = prefix.parse::<u32>() {
            if n > max {
                max = n;
            }
        }
    }
    max + 1
}

fn has_md_ext(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Load the last `limit` notes from a molecule's notes directory, sorted
/// by sequence number ascending. Used by `cs observe` to render recent
/// notes. Returns an empty vec if the directory does not exist.
pub(crate) fn load_recent(notes_dir: &Path, limit: usize) -> Vec<LoadedNote> {
    let Ok(entries) = fs::read_dir(notes_dir) else {
        return Vec::new();
    };
    let mut all: Vec<LoadedNote> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !has_md_ext(&name) {
                return None;
            }
            let (prefix, _rest) = name.split_once('-')?;
            let seq: u32 = prefix.parse().ok()?;
            let content = fs::read_to_string(e.path()).ok()?;
            Some(parse_loaded_note(seq, &name, &content))
        })
        .collect();
    all.sort_by_key(|n| n.seq);
    if all.len() > limit {
        all.drain(0..all.len() - limit);
    }
    all
}

/// A lightweight projection of a note on disk — just enough to render.
pub(crate) struct LoadedNote {
    pub seq: u32,
    pub author: String,
    pub timestamp: String,
    pub body: String,
}

fn parse_loaded_note(seq: u32, file_name: &str, content: &str) -> LoadedNote {
    // Minimal YAML frontmatter parser: strip leading `---\n...\n---\n`.
    let mut author = String::new();
    let mut timestamp = String::new();
    let mut body = content.to_owned();
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let head = &rest[..end];
            rest[end + 5..].clone_into(&mut body);
            for line in head.lines() {
                if let Some(v) = line.strip_prefix("author:") {
                    v.trim().clone_into(&mut author);
                } else if let Some(v) = line.strip_prefix("timestamp:") {
                    v.trim().clone_into(&mut timestamp);
                }
            }
        }
    }
    if author.is_empty() {
        author = file_name
            .split_once('-')
            .map(|(_, rest)| rest.trim_end_matches(".md").to_owned())
            .unwrap_or_default();
    }
    LoadedNote {
        seq,
        author,
        timestamp,
        body: body.trim().to_owned(),
    }
}
