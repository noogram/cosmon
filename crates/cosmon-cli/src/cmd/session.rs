// SPDX-License-Identifier: AGPL-3.0-only

//! `cs session` — operator carnet, append-only, BLAKE3-sealed.
//!
//! Three verbs: `start`, `note`, `end`. Each session is one markdown
//! file under `.cosmon/state/sessions/`, with a YAML frontmatter on
//! open, timestamped note blocks in the body, and a YAML footer
//! carrying `ended_at`, `note_count`, and `seal: blake3:<hex>` on
//! close.
//!
//! The seal hashes the body between the frontmatter and the footer —
//! not the whole file — so appending the footer itself does not
//! invalidate the seal. The seal is a **trace, not a lock**
//! (`architectural-invariants.md` §8b): anyone with filesystem
//! access can still rewrite the file and the hash, but a lazy
//! shadow contract (silent post-hoc edit) gets flagged.
//!
//! Open-session detection is file-based: a session is *open* while
//! its file has only one YAML block (the frontmatter). `cs session
//! end` appends the closing `---`…`---` footer, making the file
//! self-identifying as sealed.
//!
//! This is the v1 surface. No TUI, no LLM, no parser. Children
//! (Inbox, Constellation, zoom-peek) will later read these files as
//! their data root.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, SecondsFormat, Utc};
use cosmon_core::session::{Cause, CauseChannel, CauseKind, SessionNote};
use cosmon_hash::Hash;

use super::Context;

/// Exit code signalling "a session is already open" (from `cs session start`).
pub const EXIT_SESSION_ALREADY_OPEN: i32 = 2;

/// Exit code signalling "no open session" (from `cs session note` / `end`).
pub const EXIT_NO_OPEN_SESSION: i32 = 3;

/// Top-level arguments for `cs session`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// Session subcommands.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Start a new session.
    Start(StartArgs),
    /// Append a timestamped note to the open session.
    Note(NoteArgs),
    /// Close the open session, optionally sealing it with BLAKE3.
    End(EndArgs),
    /// Promote session notes into `spark` molecules (via session-to-spark tick).
    Promote(PromoteArgs),
    /// Route session notes through the Tier-1 regex classifier (ADR-072).
    ///
    /// Walks a session file, computes `blake3(body)` for each note,
    /// applies the Tier-1 cascade, writes a sidecar under
    /// `.cosmon/state/sessions/.route/<sid>/<body_hash>.json`, and
    /// (when confidence warrants) nucleates a `temp:proposed` molecule
    /// via `cs nucleate`. Tiers 2–4 are future work; low-confidence
    /// notes are marked `tier4_pending` and escalate to the
    /// verdict-door.
    Route(super::route::RouteArgs),
    /// Review router-staged molecules (verdict-door).
    ///
    /// Renders `temp:proposed` molecules as a markdown review file at
    /// `.cosmon/state/sessions/.review/<sid>.md`, opens it in `$EDITOR`,
    /// and — on `--apply` — translates each `verdict:` line into a
    /// `keep` / `dismiss` / `undo` transition. Silent when nothing is
    /// pending (no editor opens). See ADR-072 §7.
    Review(super::review::ReviewArgs),
}

/// Arguments for `cs session start`.
#[derive(clap::Args)]
pub struct StartArgs {
    /// Galaxy this session belongs to (free-form label).
    #[arg(long)]
    pub galaxy: Option<String>,

    /// Root molecule(s) this session is anchored on (repeatable).
    #[arg(long = "root", value_name = "MOL_ID")]
    pub root: Vec<String>,
}

/// Arguments for `cs session note`.
#[derive(clap::Args)]
pub struct NoteArgs {
    /// Optional tag rendered alongside the timestamp (e.g. `insight`, `todo`).
    #[arg(long)]
    pub tag: Option<String>,

    /// How the note was produced. One of `direct` (human typed),
    /// `transcription` (human spoke, agent transcribed),
    /// `oracle-suggestion` (agent proposed, human accepted),
    /// `autonomous` (agent authored alone). Omit to skip the
    /// `cause:` subline entirely — the note renders in the
    /// pre-schema format. Supplying any `--cause-*` flag enables the
    /// subline with defaults `kind=direct`, `agent=null`,
    /// `channel=keyboard`.
    #[arg(long = "cause-kind", value_name = "KIND")]
    pub cause_kind: Option<String>,

    /// Identity of the mediating agent (e.g.
    /// `apfel-oracle-<host>`, `matrix:@tenant_auditor:hs`). Leave unset when
    /// `--cause-kind direct`.
    #[arg(long = "cause-agent", value_name = "AGENT")]
    pub cause_agent: Option<String>,

    /// Physical channel the note arrived on. Known variants:
    /// `keyboard`, `voice`, `matrix`, `webhook`. Any other value is
    /// accepted verbatim and round-trips as `Other(<value>)`.
    #[arg(long = "cause-channel", value_name = "CHANNEL")]
    pub cause_channel: Option<String>,

    /// Free-form note body.
    pub text: String,
}

/// Arguments for `cs session end`.
#[derive(clap::Args)]
pub struct EndArgs {
    /// Skip the BLAKE3 seal — ephemeral scratch close. By default the
    /// session body is sealed (mirrors `prompt_seal` / `briefing_seals`).
    #[arg(long = "no-seal")]
    pub no_seal: bool,
}

/// Arguments for `cs session promote`.
///
/// Wraps the `scripts/session-to-spark-tick.sh` tick with operator
/// ergonomics — the most common case (promote one specific note) is
/// `cs session promote <note_ts>`.
///
/// See [`docs/guides/session-to-spark.md`](../../../../docs/guides/session-to-spark.md)
/// for the full workflow.
#[derive(clap::Args)]
pub struct PromoteArgs {
    /// Note timestamp to promote (`HH:MM:SS`, optionally prefixed by
    /// `<session_id>@` to disambiguate across sessions). Repeatable.
    ///
    /// When omitted, behaviour depends on `--all-spark-prefix`
    /// (default: on) and `--dry-run`.
    pub note_timestamps: Vec<String>,

    /// Session file stem (e.g. `session-2026-04-22T10-31-31Z`) or an
    /// absolute path. Defaults to the currently open session; if no
    /// session is open, defaults to scanning every session file.
    #[arg(long)]
    pub session: Option<String>,

    /// Promote every note whose body begins with `!spark `. Default
    /// behaviour when no explicit timestamps are passed; when
    /// timestamps ARE passed, this is additive (promote both the
    /// prefixed notes and the explicit ones).
    #[arg(long = "all-spark-prefix")]
    pub all_spark_prefix: bool,

    /// Print what would be promoted without nucleating or writing
    /// sidecars. Forwarded to the tick script.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Override the tick script location. Defaults to walk-up
    /// discovery from `$PWD` looking for `scripts/session-to-spark-tick.sh`.
    #[arg(long, value_name = "PATH")]
    pub tick_script: Option<PathBuf>,
}

/// Dispatch a `cs session <sub>` invocation.
///
/// # Errors
/// Propagates any filesystem, git, or session-state error.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Start(a) => run_start(ctx, a),
        Sub::Note(a) => run_note(ctx, a),
        Sub::End(a) => run_end(ctx, a),
        Sub::Promote(a) => run_promote(ctx, a),
        Sub::Route(a) => super::route::run(ctx, a),
        Sub::Review(a) => super::review::run(ctx, a),
    }
}

/// Resolve the sessions directory (`.cosmon/state/sessions/`).
fn sessions_dir(ctx: &Context) -> PathBuf {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    state_dir.join("sessions")
}

/// Format a UTC timestamp as `YYYY-MM-DDTHH-MM-SSZ` (filesystem-safe).
fn fs_timestamp(now: DateTime<Utc>) -> String {
    // Colons break filesystems on Windows and confuse shell globs; use
    // hyphens in the time section instead.
    now.format("%Y-%m-%dT%H-%M-%SZ").to_string()
}

/// Find the currently open session file in `dir`, if any.
///
/// An *open* session is one whose file contains exactly one YAML
/// block (the frontmatter) and no closing footer. We detect that by
/// checking whether the file has fewer than four `---` line markers
/// at column zero (a sealed file has four: two for the frontmatter,
/// two for the footer).
pub(crate) fn find_open_session(dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read sessions dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| anyhow::anyhow!("read sessions entry: {e}"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("session-") || !has_md_extension(&path) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read session {}: {e}", path.display()))?;
        if is_sealed(&content) {
            continue;
        }
        candidates.push(path);
    }
    if candidates.len() > 1 {
        candidates.sort();
        anyhow::bail!(
            "multiple unsealed sessions found; resolve by hand: {}",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(candidates.into_iter().next())
}

/// True when the file content already has a closing footer (four `---` markers).
fn is_sealed(content: &str) -> bool {
    content.lines().filter(|l| *l == "---").count() >= 4
}

/// Case-insensitive `.md` extension check — clippy would flag a raw
/// `ends_with(".md")` for locale sensitivity on Windows.
fn has_md_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Resolve the operator identity. Prefers `$USER`, falls back to
/// `git config user.name`, then `unknown`.
fn resolve_operator() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.trim().is_empty() {
            return u;
        }
    }
    let output = Command::new("git").args(["config", "user.name"]).output();
    if let Ok(out) = output {
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    "unknown".to_owned()
}

/// Implementation of `cs session start`.
fn run_start(ctx: &Context, args: &StartArgs) -> anyhow::Result<()> {
    let dir = sessions_dir(ctx);
    fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("create sessions dir {}: {e}", dir.display()))?;

    if let Some(existing) = find_open_session(&dir)? {
        return Err(anyhow::Error::new(SessionExit {
            code: EXIT_SESSION_ALREADY_OPEN,
            message: format!(
                "a session is already open: {} — close it first with `cs session end`",
                existing.display()
            ),
        }));
    }

    let now = Utc::now();
    let ts = fs_timestamp(now);
    let session_id = format!("session-{ts}");
    let path = dir.join(format!("{session_id}.md"));

    let operator = resolve_operator();
    let galaxy = args.galaxy.clone().unwrap_or_default();

    let mut frontmatter = String::new();
    frontmatter.push_str("---\n");
    let _ = writeln!(frontmatter, "session_id: {session_id}");
    let _ = writeln!(
        frontmatter,
        "started_at: {}",
        now.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    let _ = writeln!(frontmatter, "operator: {operator}");
    if galaxy.is_empty() {
        frontmatter.push_str("galaxy: \"\"\n");
    } else {
        let _ = writeln!(frontmatter, "galaxy: {galaxy}");
    }
    if args.root.is_empty() {
        frontmatter.push_str("root_molecules: []\n");
    } else {
        frontmatter.push_str("root_molecules:\n");
        for r in &args.root {
            let _ = writeln!(frontmatter, "  - {r}");
        }
    }
    frontmatter.push_str("---\n\n");

    fs::write(&path, frontmatter)
        .map_err(|e| anyhow::anyhow!("write session {}: {e}", path.display()))?;

    if ctx.json {
        let out = serde_json::json!({
            "session_id": session_id,
            "path": path.to_string_lossy(),
            "started_at": now.to_rfc3339_opts(SecondsFormat::Secs, true),
            "operator": operator,
            "galaxy": galaxy,
            "root_molecules": args.root,
        });
        println!("{out}");
    } else {
        println!("{session_id}");
    }
    Ok(())
}

/// Implementation of `cs session note`.
fn run_note(ctx: &Context, args: &NoteArgs) -> anyhow::Result<()> {
    let dir = sessions_dir(ctx);
    let Some(path) = find_open_session(&dir)? else {
        return Err(anyhow::Error::new(SessionExit {
            code: EXIT_NO_OPEN_SESSION,
            message: "no open session — run `cs session start` first".to_owned(),
        }));
    };

    let now = Utc::now();
    let tag = args.tag.as_deref().unwrap_or("").trim().to_owned();
    let cause = build_cause(args)?;
    let note = SessionNote::new(now, tag.clone(), args.text.clone(), cause.clone());
    let block = note.render();

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| anyhow::anyhow!("open session {}: {e}", path.display()))?;
    file.write_all(block.as_bytes())
        .map_err(|e| anyhow::anyhow!("append note: {e}"))?;

    if ctx.json {
        let cause_json = cause.as_ref().map(|c| {
            serde_json::json!({
                "kind": c.kind.as_str(),
                "agent": c.agent,
                "channel": c.channel.as_str(),
            })
        });
        let out = serde_json::json!({
            "path": path.to_string_lossy(),
            "timestamp": now.to_rfc3339_opts(SecondsFormat::Secs, true),
            "tag": tag,
            "cause": cause_json,
        });
        println!("{out}");
    } else {
        println!("note appended to {}", path.display());
    }
    Ok(())
}

/// Build an optional [`Cause`] from the CLI flags. Returns `None`
/// when no `--cause-*` flag is supplied (preserves the pre-schema
/// markdown format). When any flag is supplied, the triple is
/// completed with defaults (`kind=direct`, `agent=None`,
/// `channel=keyboard`).
fn build_cause(args: &NoteArgs) -> anyhow::Result<Option<Cause>> {
    if args.cause_kind.is_none() && args.cause_agent.is_none() && args.cause_channel.is_none() {
        return Ok(None);
    }
    let kind = match args.cause_kind.as_deref() {
        None => CauseKind::Direct,
        Some(s) => CauseKind::parse(s).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --cause-kind {s:?}: expected one of direct, transcription, oracle-suggestion, autonomous"
            )
        })?,
    };
    let agent = args.cause_agent.clone();
    let channel = args
        .cause_channel
        .as_deref()
        .map_or(CauseChannel::Keyboard, CauseChannel::parse);
    Ok(Some(Cause {
        kind,
        agent,
        channel,
    }))
}

/// Implementation of `cs session end`.
fn run_end(ctx: &Context, args: &EndArgs) -> anyhow::Result<()> {
    let dir = sessions_dir(ctx);
    let Some(path) = find_open_session(&dir)? else {
        return Err(anyhow::Error::new(SessionExit {
            code: EXIT_NO_OPEN_SESSION,
            message: "no open session".to_owned(),
        }));
    };

    let content = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read session {}: {e}", path.display()))?;

    let (body, note_count) = extract_body_for_seal(&content)?;
    let now = Utc::now();

    let seal_line = if args.no_seal {
        None
    } else {
        let hash = Hash::of_bytes(body.as_bytes());
        Some(format!("seal: blake3:{hash}"))
    };

    // Ensure exactly one blank line separates body from footer.
    let mut new_content = content.trim_end_matches('\n').to_owned();
    new_content.push_str("\n\n---\n");
    let _ = writeln!(
        new_content,
        "ended_at: {}",
        now.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    let _ = writeln!(new_content, "note_count: {note_count}");
    if let Some(line) = &seal_line {
        new_content.push_str(line);
        new_content.push('\n');
    }
    new_content.push_str("---\n");

    fs::write(&path, &new_content)
        .map_err(|e| anyhow::anyhow!("write session {}: {e}", path.display()))?;

    let session_id = session_id_from_path(&path);
    let commit_sha = auto_commit_session(&path, &session_id, note_count)
        .ok()
        .flatten();

    if ctx.json {
        let out = serde_json::json!({
            "session_id": session_id,
            "path": path.to_string_lossy(),
            "ended_at": now.to_rfc3339_opts(SecondsFormat::Secs, true),
            "note_count": note_count,
            "seal": seal_line.as_deref().unwrap_or(""),
            "commit": commit_sha,
        });
        println!("{out}");
    } else {
        println!(
            "sealed {} ({} note{})",
            path.display(),
            note_count,
            if note_count == 1 { "" } else { "s" }
        );
        if let Some(sha) = &commit_sha {
            println!("committed {sha}");
        }
    }
    Ok(())
}

/// Split the open session content into the body (between frontmatter
/// and footer) and the note count. For an open session, the footer
/// does not exist yet, so "body" is everything after the frontmatter.
///
/// Returns `(body_slice, note_count)`. The returned slice has a
/// trailing newline stripped so appending a fresh footer cannot
/// double-count the separator when the body is later re-hashed.
fn extract_body_for_seal(content: &str) -> anyhow::Result<(String, usize)> {
    // Expect the file to start with `---\n`.
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("session file missing opening frontmatter"))?;
    // Find the closing frontmatter marker on its own line.
    let close = rest
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("session file missing closing frontmatter"))?;
    let after_frontmatter = &rest[close + 5..];

    // The body excludes the footer, which for an unsealed file does
    // not exist — so the whole trailing slice is the body. We trim
    // the trailing whitespace so the seal is stable across editor
    // quirks (trailing newline, blank line).
    let body = after_frontmatter.trim_end_matches(['\n', ' ', '\t']);
    let note_count = body.lines().filter(|l| l.starts_with("## ")).count();
    Ok((body.to_owned(), note_count))
}

/// Extract the `session_id` from the file path stem.
fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session-unknown")
        .to_owned()
}

/// `git add <path> && git commit -m "chore(session): <id> — <N> notes"`.
///
/// Returns `Ok(None)` when the file is not under git or the working
/// tree has nothing staged; errors bubble up as `Err`. Auto-commit is
/// best-effort — a missing repo should not block session sealing.
fn auto_commit_session(
    path: &Path,
    session_id: &str,
    note_count: usize,
) -> anyhow::Result<Option<String>> {
    let workdir = path.parent().unwrap_or(Path::new("."));
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(workdir)
        .output();
    match toplevel {
        Ok(out) if !out.status.success() => return Ok(None),
        Err(_) => return Ok(None),
        _ => {}
    }

    let add = Command::new("git")
        .arg("add")
        .arg(path)
        .current_dir(workdir)
        .output()
        .map_err(|e| anyhow::anyhow!("git add failed: {e}"))?;
    if !add.status.success() {
        anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&add.stderr));
    }

    // Did anything actually get staged? If not, skip the commit
    // rather than fail (session file may be gitignored).
    let diff = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(workdir)
        .output()
        .map_err(|e| anyhow::anyhow!("git diff --cached failed: {e}"))?;
    if diff.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }

    let message = format!("chore(session): {session_id} — {note_count} notes");
    let commit = Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(workdir)
        .output()
        .map_err(|e| anyhow::anyhow!("git commit failed: {e}"))?;
    if !commit.status.success() {
        anyhow::bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }

    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workdir)
        .output()
        .map_err(|e| anyhow::anyhow!("git rev-parse HEAD failed: {e}"))?;
    if !sha.status.success() {
        return Ok(None);
    }
    let sha_str = String::from_utf8_lossy(&sha.stdout).trim().to_owned();
    Ok(Some(sha_str))
}

/// Implementation of `cs session promote`.
///
/// Thin wrapper over `scripts/session-to-spark-tick.sh`. The operator
/// ergonomics (auto-detect the current open session, default to
/// `--all-spark-prefix` when no timestamps are passed) live here so the
/// shell script stays focused on one clear job. The script is the
/// single source of truth for the promotion mechanism itself — this
/// function translates operator intent into tick flags and forwards
/// the invocation.
///
/// Exit code 1 is mapped to [`SessionExit`] so the caller sees a
/// matching Unix exit (operator errors like "script not found" or
/// "no open session"). Exit code 2 (transient nucleate failure) is
/// surfaced verbatim.
fn run_promote(ctx: &Context, args: &PromoteArgs) -> anyhow::Result<()> {
    let tick = resolve_tick_script(args.tick_script.as_deref())?;

    let mut shell_args: Vec<String> = Vec::new();
    if ctx.json {
        shell_args.push("--json".to_owned());
    }
    if args.dry_run {
        shell_args.push("--dry-run".to_owned());
    }

    // If the operator did not pass --session, try to resolve the
    // currently open session — the overwhelming majority of
    // `cs session promote <ts>` invocations are "the session I am in
    // right now". Fall back to scanning every session file only when
    // no explicit timestamps are named.
    let resolved_session: Option<String> = if let Some(s) = args.session.as_deref() {
        Some(s.to_owned())
    } else {
        let dir = sessions_dir(ctx);
        find_open_session(&dir)?.map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned()
        })
    };
    if let Some(s) = &resolved_session {
        if !s.is_empty() {
            shell_args.push("--session".to_owned());
            shell_args.push(s.clone());
        }
    }

    if !args.note_timestamps.is_empty() {
        // Normalise entries: allow `HH:MM:SS` shorthand (bound to the
        // resolved session) as well as the fully-qualified
        // `<session_id>@<ts>` form.
        let joined = args.note_timestamps.join(",");
        shell_args.push("--promote-notes".to_owned());
        shell_args.push(joined);
    }

    // Only enable --all-spark-prefix when the operator asked for it,
    // OR when no explicit timestamps were supplied (so the default
    // `cs session promote` with no args acts like a tick). When
    // timestamps ARE supplied, assume the operator wants a targeted
    // promotion — prefix scanning would be noisy.
    if args.all_spark_prefix || args.note_timestamps.is_empty() {
        shell_args.push("--all-spark-prefix".to_owned());
    }

    let status = Command::new("bash")
        .arg(&tick)
        .args(&shell_args)
        .status()
        .map_err(|e| anyhow::anyhow!("spawn {}: {e}", tick.display()))?;

    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(anyhow::Error::new(SessionExit {
            code,
            message: format!(
                "session-to-spark tick exited with code {code} (tick: {})",
                tick.display()
            ),
        })),
        None => Err(anyhow::anyhow!(
            "session-to-spark tick killed by signal (tick: {})",
            tick.display()
        )),
    }
}

/// Locate `scripts/session-to-spark-tick.sh`.
///
/// Preference order:
/// 1. `--tick-script <PATH>` — explicit operator override.
/// 2. `$COSMON_REPO_ROOT/scripts/session-to-spark-tick.sh` — env hint.
/// 3. Walk up from `$PWD` looking for a directory that contains both
///    `.cosmon/` (project marker) and `scripts/session-to-spark-tick.sh`
///    (the repo checkout that shipped the script).
///
/// Returns a helpful error when the script cannot be found — the
/// operator may be running `cs session promote` from a deploy of the
/// binary without the scripts shipped alongside it.
fn resolve_tick_script(override_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = override_path {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        anyhow::bail!("tick script not found at override path: {}", p.display());
    }
    if let Ok(root) = std::env::var("COSMON_REPO_ROOT") {
        let candidate = Path::new(&root).join("scripts/session-to-spark-tick.sh");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let mut dir = std::env::current_dir().map_err(|e| anyhow::anyhow!("getcwd: {e}"))?;
    loop {
        let candidate = dir.join("scripts/session-to-spark-tick.sh");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(anyhow::Error::new(SessionExit {
        code: 1,
        message: "scripts/session-to-spark-tick.sh not found (walked up from $PWD). \
                  Pass --tick-script <PATH> or set $COSMON_REPO_ROOT."
            .to_owned(),
    }))
}

/// Typed session error carrying a Unix exit code.
///
/// Returned as the outermost `anyhow::Error` so `main` can downcast
/// it and exit with the right code (2 for "already open", 3 for
/// "no open session"). This mirrors the pattern used by
/// [`super::guard::GuardError`].
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SessionExit {
    /// Unix exit code to surface on stderr-only failure.
    pub code: i32,
    /// Human-readable message (matches the original `anyhow!` text).
    pub message: String,
}

/// Extract a session exit code from an `anyhow::Error`, if the
/// outermost layer is a [`SessionExit`]. Returns `None` otherwise.
#[must_use]
pub fn extract_exit_code(err: &anyhow::Error) -> Option<i32> {
    err.downcast_ref::<SessionExit>().map(|s| s.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sealed_detects_four_markers() {
        let sealed = "---\na: 1\n---\n\n## note\n\nbody\n\n---\nended_at: X\n---\n";
        assert!(is_sealed(sealed));

        let open = "---\na: 1\n---\n\n## note\n\nbody\n";
        assert!(!is_sealed(open));
    }

    #[test]
    fn extract_body_counts_notes() {
        let content = "---\na: 1\n---\n\n## 10:00:00 — foo\n\nhello\n\n## 10:01:00 — \n\nworld\n\n";
        let (body, count) = extract_body_for_seal(content).unwrap();
        assert_eq!(count, 2);
        assert!(body.contains("hello"));
        assert!(body.contains("world"));
        assert!(!body.ends_with('\n'));
    }

    #[test]
    fn fs_timestamp_is_filesystem_safe() {
        let t = DateTime::parse_from_rfc3339("2026-04-22T14:30:05Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(fs_timestamp(t), "2026-04-22T14-30-05Z");
    }

    #[test]
    fn resolve_tick_script_override_missing_errors() {
        let missing = std::path::PathBuf::from("/definitely/not/here.sh");
        let err = resolve_tick_script(Some(&missing)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found"), "unexpected error: {msg}");
    }

    #[test]
    fn resolve_tick_script_override_found() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake-tick.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\n").unwrap();
        let got = resolve_tick_script(Some(&script)).unwrap();
        assert_eq!(got, script);
    }

    #[test]
    fn resolve_tick_script_walks_up_to_scripts_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let scripts = tmp.path().join("scripts");
        std::fs::create_dir(&scripts).unwrap();
        let script = scripts.join("session-to-spark-tick.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\n").unwrap();

        // Cross-compatibility: skip when CI disallows CWD changes.
        let orig = std::env::current_dir().ok();
        let sub = tmp.path().join("deep").join("nest");
        std::fs::create_dir_all(&sub).unwrap();
        std::env::set_current_dir(&sub).unwrap();

        let got = resolve_tick_script(None).unwrap();

        if let Some(p) = orig {
            let _ = std::env::set_current_dir(p);
        }
        // Canonicalise both sides to unwrap macOS `/private/var/folders`
        // symlink prefix — `tempdir()` and `set_current_dir` disagree
        // about the prefix on this platform and the equality check
        // fires on the literal strings otherwise.
        let got_canonical = std::fs::canonicalize(&got).unwrap();
        let expected_canonical = std::fs::canonicalize(&script).unwrap();
        assert_eq!(got_canonical, expected_canonical);
    }
}
