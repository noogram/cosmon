// SPDX-License-Identifier: AGPL-3.0-only

//! `cs whisper <mol> …` — perturbation port for a live worker.
//!
//! Injects a semantic text payload into the tmux pane of the worker that
//! owns `<mol>`. This is **not** a control-plane event: no entry is added
//! to `events.jsonl`. Instead, every successful (or refused) whisper is
//! reflogged to `MOLECULE_DIR/whispers.jsonl` with the full payload
//! content-addressed at `MOLECULE_DIR/whispers/<ts>-<sha16>.txt`.
//!
//! Safety is not optional — see `validate_target`, [`SIZE_LIMIT_BYTES`]
//! and [`RATE_LIMIT_SECONDS`]. A whisper that cannot be safely delivered
//! exits with a non-zero status and writes nothing.
//!
//! Design rationale: refuse when `pane_current_command != claude`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::PerturbationChannel;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::events::worker_spawn::emit_adapter_pane_signature_checked;
use cosmon_state::StateStore;
use cosmon_transport::claude::ADAPTER_NAME as CLAUDE_ADAPTER;
use cosmon_transport::codex::ADAPTER_NAME as CODEX_ADAPTER;
use cosmon_transport::registry::{default_registry, pane_current_command};
use sha2::{Digest, Sha256};

use super::Context;

/// Adapters whose live worker keeps a driveable interactive TUI pane open,
/// so `cs whisper` can steer them. `claude` has always been whisperable;
/// `codex` joins it once its interactive mode is the default
/// (task-20260711-246d). The pane-signature gate accepts the union of these
/// adapters' registered signatures. `aider` / `opencode` are batch
/// (fire-and-forget) and deliberately absent.
const WHISPERABLE_ADAPTERS: &[&str] = &[CLAUDE_ADAPTER, CODEX_ADAPTER];

/// Maximum payload size in bytes (8 KiB). A whisper is a nudge, not a
/// document — larger payloads almost certainly indicate misuse.
pub const SIZE_LIMIT_BYTES: usize = 8 * 1024;

/// Minimum wall-clock gap between two whispers targeting the same
/// molecule, in seconds. Enforced by reading the last line of the
/// per-molecule `whispers.jsonl`.
pub const RATE_LIMIT_SECONDS: i64 = 2;

/// Structured errors with dedicated exit codes so callers can branch on
/// `$?`. See the binary's `main` for the exit-code mapping.
#[derive(Debug)]
pub enum WhisperError {
    /// Payload exceeded [`SIZE_LIMIT_BYTES`]. Exit 2.
    MessageTooLarge { size: usize },
    /// Target pane not running an allowed command. Exit 3.
    SessionMismatch {
        session: String,
        observed: String,
        allowed: Vec<String>,
    },
    /// Too soon after the previous whisper. Exit 4.
    RateLimited {
        last_ts: DateTime<Utc>,
        min_gap_secs: i64,
    },
}

impl std::fmt::Display for WhisperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MessageTooLarge { size } => write!(
                f,
                "payload is {size} bytes, exceeds limit of {SIZE_LIMIT_BYTES} bytes"
            ),
            Self::SessionMismatch {
                session,
                observed,
                allowed,
            } => write!(
                f,
                "session {session} has pane_current_command={observed}; expected one of {allowed:?}",
            ),
            Self::RateLimited {
                last_ts,
                min_gap_secs,
            } => write!(
                f,
                "rate-limited: last whisper at {last_ts} (min gap {min_gap_secs}s)"
            ),
        }
    }
}

impl std::error::Error for WhisperError {}

impl WhisperError {
    /// Exit code reserved for this variant.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::MessageTooLarge { .. } => 2,
            Self::SessionMismatch { .. } => 3,
            Self::RateLimited { .. } => 4,
        }
    }
}

/// Arguments for the `whisper` subcommand.
///
/// Two mutually-exclusive destination types:
/// - **Molecule (tmux perturbation port, v0, ADR-038).** Positional
///   `<molecule_id>` targets a live worker whose session name is
///   `mol.session_name` (or the molecule id). The payload is pasted
///   into the tmux pane after a strict target check.
/// - **Session (log channel, v0, C-WHISPER-SESSION).** `--to-session
///   <sid>` appends one line to `.cosmon/state/presence/<sid>.log`.
///   The target reads its own log via `cs presence poll` on the next
///   turn — filesystem-based delivery, no tmux paste, no validation
///   beyond UTF-8 and the size cap. The whisper is the bit; the log
///   file is the content.
///
/// Exactly one of `<molecule_id>` and `--to-session` is required.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule whose worker pane will receive the whisper.
    ///
    /// Optional because `--to-session` is an alternative destination;
    /// enforced at runtime so clap's error text points at the mutual
    /// exclusion rather than an unsatisfied positional.
    pub molecule_id: Option<String>,

    /// Target a Claude session by id — appends one line to
    /// `.cosmon/state/presence/<sid>.log` instead of pasting into a
    /// tmux pane. Mutually exclusive with the positional
    /// `<molecule_id>`.
    ///
    /// CEILING: whispers accumulate in the log; beyond ~10 per session
    /// the signal drowns in the tail. Past that, fall back to
    /// `cs drop` / `cs tail`.
    #[arg(long = "to-session", value_name = "SID")]
    pub to_session: Option<String>,

    /// Inline payload. Mutually exclusive with `--file` / `--stdin`.
    #[arg(long, short = 'm', value_name = "TEXT")]
    pub message: Option<String>,

    /// Read payload from a file. Mutually exclusive with `--message` / `--stdin`.
    #[arg(long, short = 'f', value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Read payload from stdin (conventional `-`). Mutually exclusive with
    /// `--message` / `--file`.
    #[arg(long)]
    pub stdin: bool,

    /// Validate and log the payload without actually pasting into tmux.
    ///
    /// In `--to-session` mode, skips the append (and the seek bump) —
    /// useful for CI-style sanity checks.
    #[arg(long)]
    pub dry_run: bool,
}

/// Execute the `whisper` command.
///
/// Dispatches to [`run_to_session`] (`--to-session <SID>`) or
/// [`run_to_molecule`] (positional `<molecule_id>`) after enforcing
/// mutual exclusion. Shared payload reading + size-capping live here
/// so both destinations share the same guardrails.
///
/// # Errors
/// Fails on invalid arguments, failed I/O, refused target, size/rate limits.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    print_first_use_warning();

    // Enforce mutually-exclusive destinations at runtime so clap's error
    // message points at the logical constraint instead of complaining
    // about a missing positional.
    let raw_mol_id = match (args.molecule_id.as_deref(), args.to_session.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(anyhow::anyhow!(
                "--to-session and <molecule_id> are mutually exclusive — choose one destination"
            ));
        }
        (None, None) => {
            return Err(anyhow::anyhow!(
                "whisper needs a destination: pass <molecule_id> or --to-session <SID>"
            ));
        }
        (None, Some(sid)) => {
            let payload = read_payload(args)?;
            if payload.len() > SIZE_LIMIT_BYTES {
                return fail(
                    ctx,
                    &WhisperError::MessageTooLarge {
                        size: payload.len(),
                    },
                );
            }
            return run_to_session(ctx, sid, &payload, args.dry_run);
        }
        (Some(m), None) => m,
    };

    let payload = read_payload(args)?;
    if payload.len() > SIZE_LIMIT_BYTES {
        return fail(
            ctx,
            &WhisperError::MessageTooLarge {
                size: payload.len(),
            },
        );
    }

    run_to_molecule(ctx, raw_mol_id, &payload, args.dry_run)
}

/// Deliver a whisper to a live worker's tmux pane (v0, ADR-038).
///
/// See [`run`] for dispatch rationale. Separated out to keep the two
/// destinations balanced: filesystem-delivery on one side, tmux-paste
/// on the other, both fronted by the same payload-reading guardrails.
///
/// # Errors
/// Rate-limited if another whisper landed within [`RATE_LIMIT_SECONDS`];
/// refused if the target pane's foreground command is not in the
/// allowlist; other I/O errors bubble up as-is.
fn run_to_molecule(
    ctx: &Context,
    raw_mol_id: &str,
    payload: &[u8],
    dry_run: bool,
) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);
    let mol_id = MoleculeId::new(raw_mol_id).map_err(|e| anyhow::anyhow!("invalid id: {e}"))?;
    let mol = store
        .load_molecule(&mol_id)
        .map_err(|e| anyhow::anyhow!("failed to load molecule: {e}"))?;
    let session_name = mol
        .session_name
        .clone()
        .unwrap_or_else(|| mol_id.to_string());
    let socket = super::tmux_socket_name(ctx);
    let mol_dir = store.molecule_dir(&mol_id);
    let whispers_jsonl = mol_dir.join("whispers.jsonl");

    // Rate-limit check — read last entry of whispers.jsonl, if any.
    if let Some(last_ts) = last_whisper_ts(&whispers_jsonl) {
        let now = Utc::now();
        let gap = now.signed_duration_since(last_ts).num_seconds();
        if gap < RATE_LIMIT_SECONDS {
            return fail(
                ctx,
                &WhisperError::RateLimited {
                    last_ts,
                    min_gap_secs: RATE_LIMIT_SECONDS,
                },
            );
        }
    }

    if let Err(e) = check_pane_signature(ctx, &mol_id, &mol_dir, &socket, &session_name) {
        return fail(ctx, &e);
    }

    let sha256 = sha256_hex(payload);
    let ts = Utc::now();
    let ts_str = ts.format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let size_bytes = payload.len();
    let pilot = detect_pilot();

    if !dry_run {
        // Persist the payload (content-addressed), then append the reflog
        // line, then paste. Order matters: if the paste fails the record
        // still exists — but rate-limiting relies on the jsonl entry being
        // present so we write it before pasting.
        let payloads_dir = mol_dir.join("whispers");
        fs::create_dir_all(&payloads_dir)
            .map_err(|e| anyhow::anyhow!("failed to create whispers dir: {e}"))?;
        let sha16 = &sha256[..16];
        let payload_file = payloads_dir.join(format!("{ts_str}-{sha16}.txt"));
        fs::write(&payload_file, payload)
            .map_err(|e| anyhow::anyhow!("failed to write payload: {e}"))?;

        append_reflog(
            &whispers_jsonl,
            &ReflogEntry {
                ts,
                pilot: &pilot,
                target_session: &session_name,
                sha256: &sha256,
                size_bytes,
            },
        )?;

        let backend = cosmon_transport::TmuxBackend::new(&socket);
        let wid = WorkerId::new(&session_name)
            .map_err(|e| anyhow::anyhow!("invalid worker id {session_name}: {e}"))?;
        let payload_str = std::str::from_utf8(payload).map_err(|_| {
            anyhow::anyhow!("payload is not valid UTF-8 — tmux paste requires text")
        })?;
        backend
            .send_input(&wid, payload_str)
            .map_err(|e| anyhow::anyhow!("send_input failed: {e}"))?;
    }

    if ctx.json {
        let out = serde_json::json!({
            "delivered": !dry_run,
            "session": session_name,
            "molecule_id": mol_id.as_str(),
            "bytes": size_bytes,
            "ts": ts.to_rfc3339(),
            "sha256": sha256,
        });
        println!("{out}");
    } else if dry_run {
        println!("dry-run: would whisper {size_bytes}B to {session_name} (sha256={sha256})");
    } else {
        println!("whispered {size_bytes}B to {session_name} (sha256={sha256})");
    }
    Ok(())
}

/// Report a structured whisper failure, emitting JSON or plaintext as
/// requested and exiting with the dedicated non-zero status code.
fn fail(ctx: &Context, err: &WhisperError) -> anyhow::Result<()> {
    let code = err.exit_code();
    let kind = match &err {
        WhisperError::MessageTooLarge { .. } => "message_too_large",
        WhisperError::SessionMismatch { .. } => "session_mismatch",
        WhisperError::RateLimited { .. } => "rate_limited",
    };
    if ctx.json {
        let out = serde_json::json!({
            "error": kind,
            "message": err.to_string(),
        });
        eprintln!("{out}");
    } else {
        eprintln!("cs whisper: {err}");
    }
    std::process::exit(code);
}

/// Resolve the payload bytes according to the selected input mode.
///
/// Exactly one of `--message`, `--file`, or `--stdin` must be supplied.
fn read_payload(args: &Args) -> anyhow::Result<Vec<u8>> {
    let count = usize::from(args.message.is_some())
        + usize::from(args.file.is_some())
        + usize::from(args.stdin);
    if count != 1 {
        return Err(anyhow::anyhow!(
            "exactly one of --message, --file, or --stdin is required"
        ));
    }
    if let Some(text) = &args.message {
        return Ok(text.as_bytes().to_vec());
    }
    if let Some(path) = &args.file {
        return fs::read(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()));
    }
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;
    Ok(buf)
}

/// Run the pane-signature gate (ADR-097 PR-2 / ADR-038 §5/§6).
///
/// Builds the allowlist from the in-code `PaneSignatureRegistry` union'd
/// with the legacy `[whisper].allowed_commands` config entry (so an
/// operator override remains effective during the C3 → C6 transition).
/// Queries the live pane via [`pane_current_command`], emits an
/// `AdapterPaneSignatureChecked` event on every check (pass or fail) so
/// galileo §2.3's audit query surfaces silent mismatches, and returns
/// a [`WhisperError::SessionMismatch`] when the observed pane command is
/// outside the allowlist.
fn check_pane_signature(
    ctx: &Context,
    mol_id: &MoleculeId,
    mol_dir: &Path,
    socket: &str,
    session_name: &str,
) -> Result<(), WhisperError> {
    let config_path = super::resolve_config_from_context(ctx);
    let registry = default_registry();
    // Whisper steers a *live interactive* worker — the two tmux-pane adapters
    // that keep a driveable TUI open: `claude` and, since task-20260711-246d,
    // `codex` in its interactive mode (parity with claude). The allowlist is
    // the union of both adapters' pane signatures, so `cs whisper` reaches a
    // codex worker (`pane_current_command` = `codex` / `codex*` / `node`)
    // exactly as it reaches a claude one. `aider` / `opencode` run
    // fire-and-forget and are intentionally not whisperable.
    let mut allowed_commands: Vec<String> = Vec::new();
    for adapter in WHISPERABLE_ADAPTERS {
        for sig in registry.signatures_of(adapter) {
            if !allowed_commands.contains(sig) {
                allowed_commands.push(sig.clone());
            }
        }
    }
    if let Ok(cfg) = cosmon_filestore::load_project_config(&config_path) {
        for cmd in cfg.whisper.allowed_commands {
            if !allowed_commands.contains(&cmd) {
                allowed_commands.push(cmd);
            }
        }
    }

    let observed = pane_current_command(socket, session_name).unwrap_or_default();
    let matched = WHISPERABLE_ADAPTERS
        .iter()
        .any(|a| registry.matches(a, &observed))
        || (!observed.is_empty() && allowed_commands.iter().any(|c| c == &observed));
    if let Ok(worker_id) = WorkerId::new(session_name) {
        emit_adapter_pane_signature_checked(
            mol_dir,
            mol_id,
            &worker_id,
            // The event's nominal adapter stays `claude` (the historical
            // whisper subject); the unioned allowlist is carried in the
            // `registered_signature` field so the galileo §2.3 audit query
            // sees the full set actually checked.
            CLAUDE_ADAPTER,
            &allowed_commands,
            &observed,
            matched,
            PerturbationChannel::Whisper,
        );
    }
    if matched {
        Ok(())
    } else {
        Err(WhisperError::SessionMismatch {
            session: session_name.to_owned(),
            observed: if observed.is_empty() {
                "<missing>".to_owned()
            } else {
                observed
            },
            allowed: allowed_commands,
        })
    }
}

/// A single line in `whispers.jsonl` — fact only, no payload body.
struct ReflogEntry<'a> {
    ts: DateTime<Utc>,
    pilot: &'a str,
    target_session: &'a str,
    sha256: &'a str,
    size_bytes: usize,
}

/// Append one JSON line to `whispers.jsonl`, creating parent dirs as needed.
fn append_reflog(path: &Path, entry: &ReflogEntry<'_>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create whispers parent dir: {e}"))?;
    }
    let line = serde_json::json!({
        "ts": entry.ts.to_rfc3339(),
        "pilot": entry.pilot,
        "target_session": entry.target_session,
        "sha256": entry.sha256,
        "size_bytes": entry.size_bytes,
    })
    .to_string();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| anyhow::anyhow!("failed to open whispers.jsonl: {e}"))?;
    writeln!(f, "{line}").map_err(|e| anyhow::anyhow!("failed to append whispers.jsonl: {e}"))?;
    Ok(())
}

/// Read the timestamp of the most recent whisper entry, if any.
///
/// Returns `None` if the file is missing, empty, or the last line cannot
/// be parsed — rate-limiting is best-effort and we never want a corrupt
/// log to brick the command.
pub fn last_whisper_ts(path: &Path) -> Option<DateTime<Utc>> {
    let content = fs::read_to_string(path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(last).ok()?;
    let ts = v.get("ts")?.as_str()?;
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Hex-encoded SHA-256 of `payload`.
pub fn sha256_hex(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
}

/// Infer the pilot identity for reflog attribution.
fn detect_pilot() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Print the experimental-status banner to stderr the first time this
/// machine invokes `cs whisper`. The sentinel file lives at
/// `~/.cache/cosmon/whisper-warning-seen` (touched on first print).
fn print_first_use_warning() {
    let Some(path) = warning_sentinel_path() else {
        return;
    };
    if path.exists() {
        return;
    }
    eprintln!(
        "cs whisper is currently experimental (stable semantics target v0.5).\n\
         It is advisory, has no delivery acknowledgement, and is Propelled-regime only."
    );
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, "");
}

fn warning_sentinel_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("cosmon").join("whisper-warning-seen"))
}

/// Append one whisper line to `.cosmon/state/presence/<sid>.log`.
///
/// This is the filesystem delivery variant (C-WHISPER-SESSION). Unlike
/// the tmux port, it skips pane validation and rate-limiting: the log
/// file *is* the content channel, and the read side controls its own
/// pacing through `cs presence poll`. The whisper is advisory — pilot
/// to live session, no acknowledgement.
///
/// The line format is:
///
/// ```text
/// <RFC3339 UTC timestamp> | from:<sender> | <payload-one-line>
/// ```
///
/// Embedded newlines in the payload are replaced with a U+23CE RETURN
/// SYMBOL glyph so one whisper stays on one line (the reader's tail
/// printer splits on `\n`).
///
/// # Errors
/// Filesystem errors on create/append. A missing or GC'd presence
/// directory is recreated — a stale session id is **not** a refusal
/// (the log file is a durable record; a future session may still
/// read it).
pub fn run_to_session(
    ctx: &Context,
    sid: &str,
    payload: &[u8],
    dry_run: bool,
) -> anyhow::Result<()> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| anyhow::anyhow!("payload is not valid UTF-8 — log channel requires text"))?;
    let one_line = flatten_newlines(text);

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    // Decode the log path from the write-path taxonomy (B7 collapse,
    // delib-20260607-aec8) rather than hand-joining `presence/<sid>.log`, so
    // the directed-whisper writer and `PresenceStore` share one layout source.
    let session_id = cosmon_core::id::SessionId::new(sid)
        .map_err(|e| anyhow::anyhow!("invalid session id {sid:?}: {e}"))?;
    let store = cosmon_filestore::PresenceStore::new(&state_dir);
    let presence_dir = store.dir().to_path_buf();
    let log_path = store.log_path(&session_id);

    let ts = Utc::now();
    let sender = std::env::var("COSMON_SESSION_ID").unwrap_or_else(|_| detect_pilot());
    let line = format!("{} | from:{} | {}\n", ts.to_rfc3339(), sender, one_line);

    let stale = !log_path.exists();

    if !dry_run {
        fs::create_dir_all(&presence_dir).map_err(|e| {
            anyhow::anyhow!(
                "failed to create presence dir {}: {e}",
                presence_dir.display()
            )
        })?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", log_path.display()))?;
        f.write_all(line.as_bytes())
            .map_err(|e| anyhow::anyhow!("failed to append {}: {e}", log_path.display()))?;
    }

    if stale && !dry_run {
        eprintln!(
            "cs whisper: target session {sid} has no presence file — created fresh log at {}",
            log_path.display()
        );
    }

    if ctx.json {
        let out = serde_json::json!({
            "delivered": !dry_run,
            "to_session": sid,
            "bytes": payload.len(),
            "ts": ts.to_rfc3339(),
            "log_path": log_path.to_string_lossy(),
            "stale_session": stale,
        });
        println!("{out}");
    } else if dry_run {
        println!(
            "dry-run: would append {}B to {}",
            payload.len(),
            log_path.display()
        );
    } else {
        println!("whispered {}B to session {sid}", payload.len());
    }
    Ok(())
}

/// Collapse any embedded line terminator in `text` to a single
/// horizontal glyph so the one-line-per-whisper invariant holds.
fn flatten_newlines(text: &str) -> String {
    // U+23CE "RETURN SYMBOL" is the standard visual marker for a
    // swallowed newline and survives round-trips through terminals
    // that split on `\n`. `\r\n` pairs get collapsed first to avoid
    // emitting two glyphs for one line break.
    text.trim_end_matches('\n')
        .replace("\r\n", "⏎ ")
        .replace(['\n', '\r'], "⏎ ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sha256_hex_stable() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn size_limit_enum_reports_exact_size() {
        let err = WhisperError::MessageTooLarge { size: 9000 };
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("9000"));
    }

    #[test]
    fn rate_limit_reads_last_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("whispers.jsonl");
        let ts1 = Utc::now() - chrono::Duration::seconds(10);
        let ts2 = Utc::now() - chrono::Duration::seconds(1);
        append_reflog(
            &path,
            &ReflogEntry {
                ts: ts1,
                pilot: "tester",
                target_session: "s",
                sha256: "a",
                size_bytes: 1,
            },
        )
        .unwrap();
        append_reflog(
            &path,
            &ReflogEntry {
                ts: ts2,
                pilot: "tester",
                target_session: "s",
                sha256: "b",
                size_bytes: 1,
            },
        )
        .unwrap();
        let last = last_whisper_ts(&path).unwrap();
        // Should match ts2 to the second.
        assert_eq!(last.timestamp(), ts2.timestamp());
    }

    #[test]
    fn content_addressed_filename_uses_sha16() {
        let payload = b"hello";
        let sha = sha256_hex(payload);
        assert_eq!(&sha[..16], "2cf24dba5fb0a30e");
    }

    #[test]
    fn flatten_newlines_collapses_all_terminators() {
        assert_eq!(flatten_newlines("a\nb"), "a⏎ b");
        assert_eq!(flatten_newlines("a\r\nb"), "a⏎ b");
        assert_eq!(flatten_newlines("a\rb"), "a⏎ b");
        assert_eq!(flatten_newlines("a\n"), "a"); // trailing newline stripped
        assert_eq!(flatten_newlines("one line"), "one line");
    }

    #[test]
    fn run_to_session_appends_line_and_creates_dir() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        run_to_session(&ctx, "session-test", b"hello", false).unwrap();

        let log_path = dir.path().join("presence/session-test.log");
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.ends_with('\n'), "log lines must end with \\n");
        assert_eq!(contents.lines().count(), 1);
        let line = contents.lines().next().unwrap();
        assert!(line.contains("| from:"));
        assert!(line.ends_with("| hello"));
    }

    #[test]
    fn run_to_session_appends_idempotently() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        run_to_session(&ctx, "session-a", b"first", false).unwrap();
        run_to_session(&ctx, "session-a", b"second", false).unwrap();
        let log_path = dir.path().join("presence/session-a.log");
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.lines().next().unwrap().ends_with("| first"));
        assert!(contents.lines().nth(1).unwrap().ends_with("| second"));
    }

    #[test]
    fn run_to_session_dry_run_writes_nothing() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        run_to_session(&ctx, "session-test", b"ignored", true).unwrap();
        let log_path = dir.path().join("presence/session-test.log");
        assert!(!log_path.exists(), "dry-run must not create the log");
    }

    #[test]
    fn run_to_session_flattens_multiline_payload() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        run_to_session(&ctx, "session-x", b"line1\nline2\nline3", false).unwrap();
        let log_path = dir.path().join("presence/session-x.log");
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(
            contents.lines().count(),
            1,
            "multi-line payload must fit one log line"
        );
        assert!(contents.contains("line1⏎ line2⏎ line3"));
    }

    #[test]
    fn run_to_session_rejects_non_utf8() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        let bad = &[0xFF, 0xFE, 0xFD];
        let err = run_to_session(&ctx, "session-test", bad, false).unwrap_err();
        assert!(err.to_string().contains("UTF-8"));
    }

    /// Build an `Args` populated with sensible defaults for testing.
    /// Keeps the test matrix declarative — each test sets only the
    /// fields it cares about.
    fn bare_args() -> Args {
        Args {
            molecule_id: None,
            to_session: None,
            message: Some("hi".to_owned()),
            file: None,
            stdin: false,
            dry_run: true,
        }
    }

    #[test]
    fn run_rejects_both_destinations() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        let mut args = bare_args();
        args.molecule_id = Some("task-deadbeef".to_owned());
        args.to_session = Some("session-x".to_owned());
        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn run_rejects_no_destination() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        let args = bare_args();
        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("needs a destination"));
    }

    #[test]
    fn run_to_session_via_run_dispatches() {
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        let mut args = bare_args();
        args.to_session = Some("session-dispatch".to_owned());
        args.dry_run = false;
        run(&ctx, &args).unwrap();
        let log = dir.path().join("presence/session-dispatch.log");
        assert!(log.exists(), "--to-session must create the log via run()");
    }

    /// ADR-097 PR-2 — the gate's default allowlist is sourced from
    /// the pane-signature registry, not a hard-coded literal. The
    /// claude Adapter is registered with `["claude", "claude*",
    /// "node", "<version>"]` (the version sentinel was added
    /// 2026-06-12 after a real worker surfaced
    /// `pane_current_command=2.1.175`). A dead socket maps to an empty
    /// observed command which the registry refuses, producing the same
    /// structured `SessionMismatch` shape callers relied on before the
    /// refactor.
    #[test]
    fn registry_default_provides_claude_signature() {
        let r = default_registry();
        let sigs: Vec<String> = r.signatures_of(CLAUDE_ADAPTER).to_vec();
        assert_eq!(
            sigs,
            vec![
                "claude".to_owned(),
                "claude*".to_owned(),
                "node".to_owned(),
                "<version>".to_owned(),
            ]
        );
        // The version-string pane a real claude worker surfaces must match.
        assert!(r.matches(CLAUDE_ADAPTER, "2.1.175"));
        // A socket that definitely has no such session — pane_current_command
        // returns None → matches() returns false → the gate refuses.
        let observed = pane_current_command(
            "cosmon-whisper-registry-test-socket-absent",
            "no-such-session",
        );
        assert!(observed.is_none());
        assert!(!r.matches(CLAUDE_ADAPTER, ""));
    }

    /// task-20260711-246d — interactive-codex whisper parity. The gate's
    /// allowlist is the union of the whisperable (interactive tmux-pane)
    /// adapters, so a live codex worker — whose pane surfaces `codex`,
    /// `codex*`, or `node` — is reachable by `cs whisper` exactly like a
    /// claude worker. This is the fix for the observed refusal
    /// `has pane_current_command=codex; expected one of [claude, claude*, node]`.
    #[test]
    fn whisperable_adapters_include_codex() {
        let r = default_registry();
        assert!(WHISPERABLE_ADAPTERS.contains(&CODEX_ADAPTER));
        // Every codex pane signature must match at least one whisperable
        // adapter — the exact `any(...)` predicate the gate evaluates.
        for observed in ["codex", "codex-cli", "codex-wrapper", "node"] {
            assert!(
                WHISPERABLE_ADAPTERS.iter().any(|a| r.matches(a, observed)),
                "codex pane signature {observed:?} must be whisperable"
            );
        }
        // A crashed-into-shell pane is still refused for every adapter.
        assert!(!WHISPERABLE_ADAPTERS.iter().any(|a| r.matches(a, "zsh")));
    }

    /// ADR-097 PR-2 / galileo §2.3 — a non-claude pane signature
    /// must produce `matched=false`, which the audit query
    /// `select(.event == "AdapterPaneSignatureChecked" and .matched == false)`
    /// surfaces as the silent-pane-signature-mismatch detection event.
    /// ADR-097 PR-2 / IFBDD — a fake (unregistered) adapter's
    /// pane-signature check emits an `AdapterPaneSignatureChecked`
    /// event with `matched: false` and `channel: Whisper` into the
    /// molecule's `events.jsonl`. This is the negative path the audit
    /// query in galileo §2.3 surfaces.
    #[test]
    fn unregistered_adapter_emits_unmatched_pane_signature_event() {
        use cosmon_core::event_v2::{Envelope, EventV2};
        let dir = tempdir().unwrap();
        let mol_dir = dir.path().to_path_buf();
        let mol_id = MoleculeId::new("task-20260517-fake").unwrap();
        let worker_id = WorkerId::new("polecat-aaaa").unwrap();
        // Empty registry — no Adapter known by name "fakeadapter".
        let registry = default_registry();
        let observed = "fakeadapter";
        let matched = registry.matches("fakeadapter", observed);
        emit_adapter_pane_signature_checked(
            &mol_dir,
            &mol_id,
            &worker_id,
            "fakeadapter",
            registry.signatures_of("fakeadapter"),
            observed,
            matched,
            PerturbationChannel::Whisper,
        );

        let raw = std::fs::read_to_string(mol_dir.join("events.jsonl")).unwrap();
        let envelopes: Vec<Envelope> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Envelope::from_line(l).expect("envelope parses"))
            .collect();
        let hit = envelopes
            .iter()
            .find_map(|e| match &e.event {
                EventV2::AdapterPaneSignatureChecked {
                    adapter_name,
                    matched,
                    channel,
                    ..
                } => Some((adapter_name.clone(), *matched, *channel)),
                _ => None,
            })
            .expect("AdapterPaneSignatureChecked must be emitted");
        assert_eq!(hit.0, "fakeadapter");
        assert!(!hit.1, "fakeadapter must not match the default registry");
        assert_eq!(hit.2, PerturbationChannel::Whisper);
    }

    #[test]
    fn registry_rejects_unknown_adapter_pane_signature() {
        // `aider` (C4 / PR-3) and `codex` (delib-20260518-5178 §S7)
        // are now both real Adapters, so the canonical "definitely
        // unknown" placeholder is a synthetic name. The test's intent
        // — "an Adapter the registry has never heard of must not
        // match any pane signature" — is preserved.
        let r = default_registry();
        assert!(!r.matches("definitely-not-an-adapter", "codex"));
        assert!(!r.matches("definitely-not-an-adapter", "claude"));
        // `claude-wrapper` is now ACCEPTED via the `claude*` prefix glob
        // (the gate must tolerate wrapper / renamed installs). A pane
        // that crashed into a shell, however, must still be refused.
        assert!(r.matches(CLAUDE_ADAPTER, "claude-wrapper"));
        assert!(!r.matches(CLAUDE_ADAPTER, "zsh"));
        assert!(r.matches(CLAUDE_ADAPTER, "claude"));
    }

    #[test]
    fn run_to_session_self_loop_allowed() {
        // Re-entry: a session writes to its own log (operator reminders).
        // Must not refuse — C-WHISPER-SESSION acceptance criterion.
        // We do NOT set $COSMON_SESSION_ID here because env mutations
        // race with parallel tests; the assertion is simply that the
        // self-targeting call succeeds and the payload lands.
        let dir = tempdir().unwrap();
        let ctx = super::super::Context {
            verbose: false,
            json: false,
            config: Some(dir.path().to_path_buf()),
        };
        run_to_session(&ctx, "self-loop", b"note to self", false).unwrap();
        let log_path = dir.path().join("presence/self-loop.log");
        let contents = std::fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("| note to self"));
    }
}
