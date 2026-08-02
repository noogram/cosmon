// SPDX-License-Identifier: AGPL-3.0-only

//! `cs presence` — live-session registry and log-channel pull.
//!
//! The presence registry (ADR-038 follow-up) lives on disk under
//! `.cosmon/state/presence/`.
//! Each live session owns one `<sid>.json` snapshot that advertises the
//! session's galaxy, cwd, pid, current molecule, and a free-form
//! headline — plus a `<sid>.log` / `<sid>.seek` pair carrying the
//! whisper pull channel.
//!
//! Six subcommands ship together:
//!
//! - `ping` — upsert this session's snapshot (C-PRESENCE-CORE).
//! - `ls` — scan the directory and render live peers.
//! - `gc` — sweep stale snapshots whose pids no longer exist.
//! - `poll` — pull new whisper log lines since the last read.
//! - `send` — deliver one traced envelope to a peer's mailbox (M2).
//! - `inbox` — read and acknowledge this session's pending envelopes (M2).
//!
//! Composition: all six share a single `PresenceStore` pointed at
//! `<state_root>/presence/`. Layout is stable — writers
//! (`cs whisper --to-session`) and readers (`cs presence poll`) can
//! share the same path helpers.
//!
//! # Two channels, on purpose
//!
//! `poll` reads the byte-cursor text channel that `cs whisper --to-session`
//! writes. `inbox` reads the [`PilotMailbox`] envelope channel that `send`
//! writes. They are not merged, and the reason is authority rather than
//! taste: the text channel has one line of text and no identity, so a retry
//! is indistinguishable from a second instruction. Merging them would make
//! the traced channel inherit the untraced one's ambiguity.
//!
//! `cs whisper <molecule>` — the worker perturbation port of ADR-038 — is a
//! third thing entirely and is untouched by any of this.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::cas::{CasStore, ContentHash};
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::pilot_message::PilotMessage;
use cosmon_core::presence::{PilotRole, Presence};
use cosmon_filestore::cas::FileCas;
use cosmon_filestore::{PilotMailbox, PresenceStore};

use super::Context;

/// Top-level arguments for `cs presence`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// Presence subcommands — see module doc for the full picture.
///
/// `PingArgs` is the widest variant by some margin (ten optional flags), and
/// clippy would rather it were boxed. It cannot be: clap's `Subcommand` derive
/// requires the variant's field to implement `Args`, which `Box<PingArgs>`
/// does not. The enum is constructed once per process from the command line,
/// so the size costs one stack frame per `cs` invocation.
#[allow(clippy::large_enum_variant)]
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Upsert this session's presence snapshot.
    Ping(PingArgs),
    /// List live sessions known to this galaxy's presence directory.
    Ls(LsArgs),
    /// Remove stale snapshots whose pids are no longer alive.
    Gc,
    /// Print unread log lines for a session and bump the seek pointer.
    Poll(PollArgs),
    /// Deliver one traced message envelope to a peer session's mailbox.
    Send(SendArgs),
    /// Read this session's pending message envelopes and acknowledge them.
    Inbox(InboxArgs),
}

/// Arguments for `cs presence ping`.
#[derive(clap::Args, Default)]
pub struct PingArgs {
    /// Session id to write. Defaults to `$COSMON_SESSION_ID`; falls
    /// back to a tty-derived stable id if unset.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// One-line description of what the session is doing.
    #[arg(long)]
    pub headline: Option<String>,
    /// Molecule currently under this session's attention.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub molecule: Option<MoleculeId>,
    /// Override the galaxy label (default: `cosmon`).
    #[arg(long, default_value = "cosmon")]
    pub galaxy: String,
    /// Provider that minted the underlying model session (`claude`,
    /// `codex`, …). Half of the `<provider>:<native-session-id>` key.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// The provider's own id for this session — from inside its log, never
    /// a display title. The other half of the key.
    #[arg(long = "native-session-id", value_name = "ID")]
    pub native_session_id: Option<String>,
    /// Seat this pilot occupies: `primary` or `copilot`. Omitted means
    /// `copilot` — read-only, per FAIL-CLOSED-AUTHORITY.
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    /// Session this pilot is co-piloting. Sets the `follows` relation that
    /// makes presence reciprocal rather than merely parallel.
    #[arg(long, value_name = "SID")]
    pub follows: Option<String>,
    /// A capability this pilot advertises (`observe`, `message`,
    /// `checkpoint`, …). Repeatable.
    #[arg(long = "capability", value_name = "TOKEN")]
    pub capabilities: Vec<String>,
    /// Most recent checkpoint published by this pilot.
    #[arg(long, value_name = "CHECKPOINT_ID")]
    pub checkpoint: Option<String>,
}

/// Arguments for `cs presence ls`.
#[derive(clap::Args, Default)]
pub struct LsArgs {
    /// Emit NDJSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
    /// Include stale (but not yet garbage-collected) snapshots.
    #[arg(long)]
    pub all: bool,
    /// Filter to one galaxy (default: show every galaxy present).
    #[arg(long)]
    pub galaxy: Option<String>,
    /// Show only pilots in this seat (`primary` or `copilot`).
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    /// Show only pilots co-piloting this session.
    #[arg(long, value_name = "SID")]
    pub follows: Option<String>,
}

/// Arguments for `cs presence send`.
#[derive(clap::Args, Default)]
pub struct SendArgs {
    /// Destination session id.
    #[arg(long, value_name = "SID")]
    pub to: String,
    /// Message body. Stored content-addressed; the envelope carries only its
    /// hash and reference.
    #[arg(long, value_name = "TEXT")]
    pub message: String,
    /// Sender session id. Defaults to `$COSMON_SESSION_ID` — the *session*,
    /// not the OS username, so two pilots on one host stay distinguishable.
    #[arg(long, value_name = "SID")]
    pub from: Option<String>,
    /// Seconds after which an unread message reads as `expired` rather than
    /// as a fresh instruction. Omitted means it never expires.
    #[arg(long = "expires-in", value_name = "SECONDS")]
    pub expires_in: Option<i64>,
}

/// Arguments for `cs presence inbox`.
#[derive(clap::Args, Default)]
pub struct InboxArgs {
    /// Session whose mailbox to read. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// Show the pending envelopes without acknowledging them. Use this to
    /// look without consuming — an unacknowledged message is redelivered.
    #[arg(long)]
    pub peek: bool,
    /// Include already-acknowledged envelopes in the listing.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `cs presence poll`.
#[derive(clap::Args, Default)]
pub struct PollArgs {
    /// Session id whose log to poll. Defaults to `$COSMON_SESSION_ID`
    /// when unset — the runtime exports this on session start.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
}

/// Dispatch a `cs presence <sub>` invocation.
///
/// # Errors
/// Propagates filesystem errors and "no session id" when the operator
/// neither passed `--session` nor exported `$COSMON_SESSION_ID`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Ping(a) => run_ping(ctx, a),
        Sub::Ls(a) => run_ls(ctx, a),
        Sub::Gc => run_gc(ctx),
        Sub::Poll(a) => run_poll(ctx, a),
        Sub::Send(a) => run_send(ctx, a),
        Sub::Inbox(a) => run_inbox(ctx, a),
    }
}

fn state_root(ctx: &Context) -> PathBuf {
    ctx.config.clone().unwrap_or_else(super::default_state_dir)
}

fn store(ctx: &Context) -> PresenceStore {
    PresenceStore::new(state_root(ctx))
}

fn mailbox(ctx: &Context) -> PilotMailbox {
    PilotMailbox::new(state_root(ctx))
}

/// Parse the `--role` token into a [`PilotRole`].
///
/// An unrecognised token is an **error**, not a fallback to `copilot`. A
/// silent fallback would turn `--role primry` into a demotion the operator
/// never sees; fail-closed means refusing the gesture, not guessing at it.
fn parse_role(raw: &str) -> anyhow::Result<PilotRole> {
    match raw {
        "primary" => Ok(PilotRole::Primary),
        "copilot" => Ok(PilotRole::Copilot),
        other => Err(anyhow::anyhow!(
            "unknown role '{other}' — expected 'primary' or 'copilot'"
        )),
    }
}

fn run_ping(ctx: &Context, args: &PingArgs) -> anyhow::Result<()> {
    let session_id = resolve_or_derive_sid(args.session.as_deref())?;
    let now = Utc::now();
    let store = store(ctx);

    // Preserve `started_at` across subsequent pings so the registry
    // records genuine session age, not last-heartbeat age. If the
    // previous snapshot is gone or corrupt we treat this as a fresh
    // start — silent fallback, not an error.
    let prior = load_prior(&store, &session_id);
    let prior_started_at = prior.as_ref().map(|p| p.started_at);

    // A ping that omits a co-pilot field carries the previous ping's value
    // forward. The heartbeat is emitted by a hook every ~30 s and must not
    // silently erase a role or a `follows` the operator set once by hand.
    let carry_str = |arg: &Option<String>, prior: Option<&String>| -> Option<String> {
        arg.clone().or_else(|| prior.cloned())
    };

    let role = match &args.role {
        Some(raw) => parse_role(raw)?,
        None => prior.as_ref().map_or_else(PilotRole::default, |p| p.role),
    };
    let follows = match &args.follows {
        Some(raw) => Some(SessionId::new(raw.clone())?),
        None => prior.as_ref().and_then(|p| p.follows.clone()),
    };
    let capabilities = if args.capabilities.is_empty() {
        prior
            .as_ref()
            .map(|p| p.capabilities.clone())
            .unwrap_or_default()
    } else {
        args.capabilities.clone()
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let presence = Presence {
        heartbeat_at: now,
        current_molecule: args.molecule.clone(),
        headline: args
            .headline
            .clone()
            .or_else(|| prior.as_ref().map(|p| p.headline.clone()))
            .unwrap_or_default(),
        tty: current_tty(),
        provider: carry_str(
            &args.provider,
            prior.as_ref().and_then(|p| p.provider.as_ref()),
        ),
        native_session_id: carry_str(
            &args.native_session_id,
            prior.as_ref().and_then(|p| p.native_session_id.as_ref()),
        ),
        role,
        follows,
        capabilities,
        checkpoint_id: carry_str(
            &args.checkpoint,
            prior.as_ref().and_then(|p| p.checkpoint_id.as_ref()),
        ),
        ..Presence::new(
            session_id.clone(),
            args.galaxy.clone(),
            cwd,
            std::process::id(),
            prior_started_at.unwrap_or(now),
        )
    };
    store.upsert(&presence)?;

    if ctx.json {
        let v = serde_json::to_value(&presence)?;
        println!("{v}");
    } else {
        let selector = presence
            .selector()
            .map_or_else(String::new, |s| format!(" [{s}]"));
        let follows = presence
            .follows
            .as_ref()
            .map_or_else(String::new, |f| format!(" follows={}", f.as_str()));
        println!(
            "presence ping: {sid} in {galaxy} (pid={pid}) role={role}{selector}{follows}",
            sid = presence.session_id.as_str(),
            galaxy = presence.galaxy,
            pid = presence.pid,
            role = presence.role.as_str(),
        );
    }
    Ok(())
}

fn run_ls(ctx: &Context, args: &LsArgs) -> anyhow::Result<()> {
    let store = store(ctx);
    let now = Utc::now();
    let mut rows = store.scan()?;
    if !args.all {
        rows.retain(|p| p.is_live(now));
    }
    if let Some(ref galaxy) = args.galaxy {
        rows.retain(|p| &p.galaxy == galaxy);
    }
    if let Some(ref raw) = args.role {
        let want = parse_role(raw)?;
        rows.retain(|p| p.role == want);
    }
    if let Some(ref sid) = args.follows {
        rows.retain(|p| p.follows.as_ref().is_some_and(|f| f.as_str() == sid));
    }
    // Stable order, oldest heartbeat last.
    rows.sort_by_key(|x| std::cmp::Reverse(x.heartbeat_at));

    if args.json || ctx.json {
        let mut out = std::io::stdout().lock();
        for p in &rows {
            let v = serde_json::to_value(p)?;
            writeln!(out, "{v}")?;
        }
        return Ok(());
    }

    if rows.is_empty() {
        println!("(no live sessions)");
        return Ok(());
    }
    let header = format!(
        "{:<26}  {:<12}  {:>6}  {:>8}  {:<8}  {:<20}  HEADLINE",
        "SESSION", "GALAXY", "PID", "AGE", "ROLE", "FOLLOWS",
    );
    println!("{header}");
    for p in &rows {
        println!(
            "{:<26}  {:<12}  {:>6}  {:>8}  {:<8}  {:<20}  {}",
            p.session_id.as_str(),
            p.galaxy,
            p.pid,
            format_age(now, p.heartbeat_at),
            p.role.as_str(),
            p.follows.as_ref().map_or("-", SessionId::as_str),
            p.headline,
        );
    }
    Ok(())
}

fn run_gc(ctx: &Context) -> anyhow::Result<()> {
    let removed = store(ctx).gc()?;
    if ctx.json {
        let v = serde_json::json!({ "removed": removed });
        println!("{v}");
    } else {
        println!("presence gc: removed {removed} stale snapshot(s)");
    }
    Ok(())
}

fn run_poll(ctx: &Context, args: &PollArgs) -> anyhow::Result<()> {
    let sid = resolve_sid_for_poll(args)?;
    let store = store(ctx);
    let log_path = store.log_path(&sid);
    let seek_path = store.seek_path(&sid);

    let content = fs::read_to_string(&log_path).unwrap_or_default();
    let end = content.len();
    let seek = clamp_seek(read_seek(&seek_path), &content);
    let tail = &content[seek..];

    if ctx.json {
        let lines: Vec<&str> = tail.lines().collect();
        let out = serde_json::json!({
            "session": sid.as_str(),
            "bytes": tail.len(),
            "lines": lines,
            "seek": end,
        });
        println!("{out}");
    } else {
        print!("{tail}");
    }

    // The seek moves only after the tail has left this process (ADR-168 §D4,
    // P3). Bumping it first makes delivery at-most-once: a reader killed
    // between the write and the print loses the text with the pointer already
    // past it. Flushing before the bump is what makes the loss impossible —
    // a crash now costs a re-read, which is the correct failure.
    std::io::stdout().flush().ok();

    if !tail.is_empty() {
        fs::create_dir_all(store.dir()).map_err(|e| {
            anyhow::anyhow!(
                "failed to create presence dir {}: {e}",
                store.dir().display()
            )
        })?;
        fs::write(&seek_path, end.to_string())
            .map_err(|e| anyhow::anyhow!("failed to write seek {}: {e}", seek_path.display()))?;
    }
    Ok(())
}

/// Bring a stored byte offset back inside `content`.
///
/// Two failure modes of a byte cursor, both recorded in ADR-168 §D4 and both
/// fixed here rather than left to the reader:
///
/// - **P4 — rotation.** A seek past the end means the log was truncated or
///   rotated under us. The honest reading is "the file I was tracking is
///   gone", so the cursor restarts at 0 and the backlog is served rather than
///   swallowed. A silently skipped backlog is a message lost with a success
///   exit code.
/// - **P5 — a seek inside a multi-byte character.** Slicing a `str` there
///   panics. The cursor is walked back to the nearest character boundary,
///   which at worst re-emits the few bytes of one character.
fn clamp_seek(seek: usize, content: &str) -> usize {
    if seek >= content.len() {
        // Equal is legitimately "nothing new"; greater is rotation. Both are
        // safe to express as "start of what is there now" only in the second
        // case, so distinguish them.
        return if seek > content.len() {
            0
        } else {
            content.len()
        };
    }
    let mut at = seek;
    while at > 0 && !content.is_char_boundary(at) {
        at -= 1;
    }
    at
}

// ---------------------------------------------------------------------------
// Pilot mailbox — the traced envelope channel (M2)
// ---------------------------------------------------------------------------

fn run_send(ctx: &Context, args: &SendArgs) -> anyhow::Result<()> {
    let to = SessionId::new(args.to.clone())?;
    let from = match &args.from {
        Some(s) => SessionId::new(s.clone())?,
        None => resolve_sid_for_poll(&PollArgs { session: None }).map_err(|_| {
            anyhow::anyhow!("no sender — pass --from <SID> or export COSMON_SESSION_ID")
        })?,
    };

    // The body is content-addressed, so the envelope stays one `jq`-readable
    // line no matter how long a checkpoint gets (ADR-168 §D5).
    let cas = FileCas::new(state_root(ctx).join("cas"));
    let hash = cas.put(args.message.as_bytes())?;

    let mailbox = mailbox(ctx);
    let now = Utc::now();
    let sequence = mailbox.next_sequence(&to)?;
    let expires_at = args
        .expires_in
        .map(|secs| now + chrono::Duration::seconds(secs));
    let message = PilotMessage::new(
        from,
        to.clone(),
        sequence,
        format!("cas/{}/{}", hash.prefix(), hash.as_str()),
        hash.as_str(),
        now,
        expires_at,
    );
    let written = mailbox.deliver(&message)?;

    if ctx.json {
        let out = serde_json::json!({
            "id": message.id.as_str(),
            "to": to.as_str(),
            "from": message.from.as_str(),
            "sequence": message.sequence,
            "payload_ref": message.payload_ref,
            "payload_hash": message.payload_hash,
            "delivered": written,
        });
        println!("{out}");
    } else if written {
        println!(
            "presence send: {id} → {to} (seq {seq})",
            id = message.id,
            to = to.as_str(),
            seq = message.sequence,
        );
    } else {
        println!(
            "presence send: {id} already in {to}'s inbox — not delivered twice",
            id = message.id,
            to = to.as_str(),
        );
    }
    Ok(())
}

fn run_inbox(ctx: &Context, args: &InboxArgs) -> anyhow::Result<()> {
    let sid = resolve_sid_for_poll(&PollArgs {
        session: args.session.clone(),
    })?;
    let mailbox = mailbox(ctx);
    let cas = FileCas::new(state_root(ctx).join("cas"));
    let now = Utc::now();

    let entries = if args.all {
        mailbox.entries(&sid, now)?
    } else {
        mailbox.pending(&sid, now)?
    };

    // Read the bodies before acknowledging anything: an ack claims the reader
    // has the text, and a body that cannot be loaded means it does not.
    let mut rendered: Vec<(&cosmon_filestore::MailboxEntry, Option<String>)> =
        Vec::with_capacity(entries.len());
    for e in &entries {
        let body = cas
            .get(&ContentHash::new(e.message.payload_hash.clone())?)
            .ok()
            .and_then(|b| String::from_utf8(b).ok());
        rendered.push((e, body));
    }

    if ctx.json {
        let mut out = std::io::stdout().lock();
        for (e, body) in &rendered {
            let v = serde_json::json!({
                "id": e.message.id.as_str(),
                "from": e.message.from.as_str(),
                "to": e.message.to.as_str(),
                "sequence": e.message.sequence,
                "state": e.state.as_str(),
                "created_at": e.message.created_at,
                "expires_at": e.message.expires_at,
                "read_at": e.read_at,
                "payload_hash": e.message.payload_hash,
                "body": body,
            });
            writeln!(out, "{v}")?;
        }
    } else if rendered.is_empty() {
        println!("(no pending messages for {})", sid.as_str());
    } else {
        for (e, body) in &rendered {
            println!(
                "[{state}] #{seq} {id} from {from}",
                state = e.state.as_str(),
                seq = e.message.sequence,
                id = e.message.id,
                from = e.message.from.as_str(),
            );
            match body {
                Some(text) => println!("  {text}"),
                None => println!("  (payload {} unreadable)", e.message.payload_hash),
            }
        }
    }

    // Acknowledge last, and only after the text has left this process — the
    // same ordering rule as `poll`, for the same reason. `--peek` skips the
    // ack entirely, which is how an operator looks without consuming.
    std::io::stdout().flush().ok();
    if !args.peek {
        for (e, body) in &rendered {
            if body.is_none() {
                // An envelope whose body we could not read has not been
                // consumed. Leaving it pending is the honest state.
                continue;
            }
            if e.read_at.is_none() {
                mailbox.ack(&sid, &e.message.id, now)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Session id resolution
// ---------------------------------------------------------------------------

/// Resolve a session id for `ping`: explicit `--session` wins, then
/// `$COSMON_SESSION_ID`, then `$CLAUDE_SESSION_ID`, then a stable
/// tty-hash fallback so two shells in different tabs get distinct ids.
fn resolve_or_derive_sid(explicit: Option<&str>) -> anyhow::Result<SessionId> {
    if let Some(s) = explicit {
        return Ok(SessionId::new(s)?);
    }
    for env_var in ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Ok(s) = std::env::var(env_var) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Ok(SessionId::new(trimmed)?);
            }
        }
    }
    Ok(SessionId::new(derive_stable_sid())?)
}

/// Resolve a session id for `poll` — same precedence as `ping` but the
/// tty-hash fallback is not used (poll is always driven by a hook that
/// already knows the id).
fn resolve_sid_for_poll(args: &PollArgs) -> anyhow::Result<SessionId> {
    if let Some(s) = &args.session {
        return Ok(SessionId::new(s.clone())?);
    }
    for env_var in ["COSMON_SESSION_ID", "CLAUDE_SESSION_ID"] {
        if let Ok(s) = std::env::var(env_var) {
            if !s.trim().is_empty() {
                return Ok(SessionId::new(s)?);
            }
        }
    }
    Err(anyhow::anyhow!(
        "no session id — pass --session <SID> or export COSMON_SESSION_ID"
    ))
}

/// Derive a stable session id from `(tty, boot_epoch)` when no
/// environment override is present. Produces `session-<12-hex>` so it
/// looks distinct from a Claude-provided UUID and survives CLI
/// invocations from the same shell.
fn derive_stable_sid() -> String {
    use sha2::{Digest, Sha256};
    let tty = current_tty().unwrap_or_else(|| "unknown-tty".to_owned());
    let boot = boot_epoch_seconds().unwrap_or(0);
    let mut h = Sha256::new();
    h.update(tty.as_bytes());
    h.update(b":");
    h.update(boot.to_le_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(12);
    for b in digest.iter().take(6) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("session-{hex}")
}

fn current_tty() -> Option<String> {
    // `tty(1)` is POSIX-standard; the output is one line such as
    // `/dev/ttys012`. When stdin is not a tty it prints "not a tty"
    // and exits 1, which we surface as `None`.
    let out = std::process::Command::new("tty").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn boot_epoch_seconds() -> Option<u64> {
    // macOS: `sysctl -n kern.boottime` → `{ sec = 1745..., usec = ... }`.
    // Linux: `/proc/stat` exposes `btime <epoch>`. Both fail gracefully:
    // a `None` here just means the sid falls back on pure-tty hashing,
    // still stable within a single boot.
    if let Ok(content) = fs::read_to_string("/proc/stat") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                if let Ok(n) = rest.trim().parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    // Parse "{ sec = 1745689200, usec = 12345 } ..." defensively.
    let sec_idx = s.find("sec = ")?;
    let tail = &s[sec_idx + "sec = ".len()..];
    let end = tail.find(',').unwrap_or(tail.len());
    tail[..end].trim().parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_prior(store: &PresenceStore, sid: &SessionId) -> Option<Presence> {
    let path = store.snapshot_path(sid);
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn read_seek(path: &Path) -> usize {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

fn format_age(now: DateTime<Utc>, heartbeat: DateTime<Utc>) -> String {
    let d = now - heartbeat;
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::presence::STALE_AFTER;
    use tempfile::tempdir;

    fn ctx_for(dir: &Path) -> Context {
        Context {
            verbose: false,
            json: false,
            config: Some(dir.to_path_buf()),
        }
    }

    #[test]
    fn read_seek_missing_returns_zero() {
        let dir = tempdir().unwrap();
        assert_eq!(read_seek(&dir.path().join("nope.seek")), 0);
    }

    #[test]
    fn read_seek_unparseable_returns_zero() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.seek");
        fs::write(&p, "not-a-number").unwrap();
        assert_eq!(read_seek(&p), 0);
    }

    #[test]
    fn read_seek_valid() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("ok.seek");
        fs::write(&p, "42").unwrap();
        assert_eq!(read_seek(&p), 42);
    }

    #[test]
    fn ping_writes_snapshot_and_ls_reads_it_back() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_ping(
            &ctx,
            &PingArgs {
                session: Some("session-alpha".to_owned()),
                headline: Some("writing tests".to_owned()),
                galaxy: "cosmon".to_owned(),
                ..PingArgs::default()
            },
        )
        .unwrap();

        let loaded = PresenceStore::new(dir.path()).scan().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id.as_str(), "session-alpha");
        assert_eq!(loaded[0].galaxy, "cosmon");
        assert_eq!(loaded[0].headline, "writing tests");

        // ls should at least run without error.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                ..LsArgs::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn ping_preserves_started_at_across_bumps() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let args = PingArgs {
            session: Some("session-sticky".to_owned()),
            galaxy: "cosmon".to_owned(),
            ..PingArgs::default()
        };
        run_ping(&ctx, &args).unwrap();
        let first = PresenceStore::new(dir.path()).scan().unwrap()[0].clone();
        std::thread::sleep(std::time::Duration::from_millis(10));
        run_ping(&ctx, &args).unwrap();
        let second = PresenceStore::new(dir.path()).scan().unwrap()[0].clone();
        assert_eq!(first.started_at, second.started_at);
        assert!(second.heartbeat_at >= first.heartbeat_at);
    }

    #[test]
    fn gc_runs_on_empty_dir() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_gc(&ctx).unwrap();
    }

    #[test]
    fn ls_all_flag_includes_stale() {
        let dir = tempdir().unwrap();
        let store = PresenceStore::new(dir.path());
        // Hand-write a stale-but-alive snapshot so we exercise the filter.
        let old = Utc::now() - STALE_AFTER - chrono::Duration::minutes(5);
        let p = Presence {
            headline: "stale".to_owned(),
            ..Presence::new(
                SessionId::new("session-stale").unwrap(),
                "cosmon",
                PathBuf::from("/tmp"),
                std::process::id(),
                old,
            )
        };
        store.upsert(&p).unwrap();

        let ctx = ctx_for(dir.path());
        // Default ls filters it out.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                ..LsArgs::default()
            },
        )
        .unwrap();
        // --all includes it.
        run_ls(
            &ctx,
            &LsArgs {
                json: true,
                all: true,
                ..LsArgs::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn poll_with_no_presence_dir_is_clean() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();
    }

    #[test]
    fn poll_emits_unread_tail_and_bumps_seek() {
        let dir = tempdir().unwrap();
        let presence = dir.path().join("presence");
        fs::create_dir_all(&presence).unwrap();
        let log = presence.join("session-test.log");
        fs::write(&log, "first\nsecond\n").unwrap();

        let ctx = ctx_for(dir.path());
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();

        let seek = fs::read_to_string(presence.join("session-test.seek")).unwrap();
        assert_eq!(seek, "first\nsecond\n".len().to_string());

        // Second poll returns nothing; seek stays put.
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-test".to_owned()),
            },
        )
        .unwrap();
        let seek2 = fs::read_to_string(presence.join("session-test.seek")).unwrap();
        assert_eq!(seek, seek2);
    }

    #[test]
    fn resolve_for_poll_prefers_explicit() {
        let args = PollArgs {
            session: Some("explicit".to_owned()),
        };
        assert_eq!(resolve_sid_for_poll(&args).unwrap().as_str(), "explicit");
    }

    #[test]
    fn derive_stable_sid_is_nonempty_and_prefixed() {
        let s = derive_stable_sid();
        assert!(s.starts_with("session-"));
        assert!(s.len() > "session-".len());
    }

    // Sanity: the presence_dir layout the CLI reads matches what the
    // PresenceStore produces. A refactor that diverges the two must
    // fail here.
    #[test]
    fn presence_log_filename_contract() {
        let store = PresenceStore::new(PathBuf::from("/tmp/state"));
        let sid = SessionId::new("session-2026-04-24T10-00-00Z").unwrap();
        assert_eq!(
            store.log_path(&sid).to_string_lossy(),
            "/tmp/state/presence/session-2026-04-24T10-00-00Z.log"
        );
        assert_eq!(
            store.snapshot_path(&sid).to_string_lossy(),
            "/tmp/state/presence/session-2026-04-24T10-00-00Z.json"
        );
    }

    #[test]
    fn format_age_labels() {
        let now = Utc::now();
        assert_eq!(format_age(now, now), "0s");
        assert_eq!(format_age(now, now - chrono::Duration::seconds(45)), "45s");
        assert_eq!(format_age(now, now - chrono::Duration::minutes(3)), "3m");
        assert_eq!(format_age(now, now - chrono::Duration::hours(2)), "2h");
    }

    // -----------------------------------------------------------------
    // M2 — reciprocal presence
    // -----------------------------------------------------------------

    fn ping(ctx: &Context, args: PingArgs) {
        run_ping(ctx, &args).unwrap();
    }

    /// The acceptance clause, in one test: Claude sees Codex and Codex sees
    /// Claude, each knowing which seat the other holds and who is following
    /// whom — from a directory scan, with no broker anywhere.
    #[test]
    fn claude_sees_codex_and_codex_sees_claude() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());

        ping(
            &ctx,
            PingArgs {
                session: Some("claude-sid".to_owned()),
                galaxy: "stagecraft".to_owned(),
                provider: Some("claude".to_owned()),
                native_session_id: Some("4940f28e".to_owned()),
                role: Some("primary".to_owned()),
                capabilities: vec!["observe".to_owned(), "mutate".to_owned()],
                ..PingArgs::default()
            },
        );
        ping(
            &ctx,
            PingArgs {
                session: Some("codex-sid".to_owned()),
                galaxy: "stagecraft".to_owned(),
                provider: Some("codex".to_owned()),
                native_session_id: Some("019823ab".to_owned()),
                role: Some("copilot".to_owned()),
                follows: Some("claude-sid".to_owned()),
                capabilities: vec!["observe".to_owned()],
                ..PingArgs::default()
            },
        );

        let seen = PresenceStore::new(dir.path()).scan().unwrap();
        let claude = seen
            .iter()
            .find(|p| p.session_id.as_str() == "claude-sid")
            .expect("codex can see claude");
        let codex = seen
            .iter()
            .find(|p| p.session_id.as_str() == "codex-sid")
            .expect("claude can see codex");

        assert_eq!(claude.selector().as_deref(), Some("claude:4940f28e"));
        assert_eq!(codex.selector().as_deref(), Some("codex:019823ab"));
        assert!(claude.role.is_primary());
        assert!(!codex.role.is_primary(), "a co-pilot is read-only");
        assert_eq!(
            codex.follows.as_ref().map(SessionId::as_str),
            Some("claude-sid"),
            "the relation is what makes the view reciprocal rather than parallel",
        );
        assert!(claude.follows.is_none(), "the primary follows nobody");
        assert_eq!(codex.capabilities, vec!["observe".to_owned()]);
    }

    // A heartbeat is emitted by a hook every ~30 s with no flags at all. If
    // it reset the seat, a primary would silently demote itself between two
    // operator gestures.
    #[test]
    fn a_bare_heartbeat_does_not_erase_the_seat_it_found() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        ping(
            &ctx,
            PingArgs {
                session: Some("pilot".to_owned()),
                role: Some("primary".to_owned()),
                provider: Some("claude".to_owned()),
                native_session_id: Some("abc".to_owned()),
                capabilities: vec!["mutate".to_owned()],
                ..PingArgs::default()
            },
        );
        ping(
            &ctx,
            PingArgs {
                session: Some("pilot".to_owned()),
                ..PingArgs::default()
            },
        );

        let p = PresenceStore::new(dir.path()).scan().unwrap()[0].clone();
        assert!(
            p.role.is_primary(),
            "the heartbeat carried the seat forward"
        );
        assert_eq!(p.selector().as_deref(), Some("claude:abc"));
        assert_eq!(p.capabilities, vec!["mutate".to_owned()]);
    }

    #[test]
    fn a_misspelt_role_is_refused_rather_than_downgraded() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let err = run_ping(
            &ctx,
            &PingArgs {
                session: Some("pilot".to_owned()),
                role: Some("primry".to_owned()),
                ..PingArgs::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown role"), "{err}");
        assert!(
            PresenceStore::new(dir.path()).scan().unwrap().is_empty(),
            "a refused gesture writes nothing",
        );
    }

    #[test]
    fn ls_filters_on_role_and_on_the_follows_relation() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        ping(
            &ctx,
            PingArgs {
                session: Some("primary-sid".to_owned()),
                role: Some("primary".to_owned()),
                ..PingArgs::default()
            },
        );
        ping(
            &ctx,
            PingArgs {
                session: Some("copilot-sid".to_owned()),
                follows: Some("primary-sid".to_owned()),
                ..PingArgs::default()
            },
        );

        for (role, follows) in [
            (Some("primary".to_owned()), None),
            (None, Some("primary-sid".to_owned())),
        ] {
            run_ls(
                &ctx,
                &LsArgs {
                    json: true,
                    role,
                    follows,
                    ..LsArgs::default()
                },
            )
            .unwrap();
        }
        // An unknown role is refused here too, not silently matched.
        assert!(run_ls(
            &ctx,
            &LsArgs {
                json: true,
                role: Some("captain".to_owned()),
                ..LsArgs::default()
            },
        )
        .is_err());
    }

    // -----------------------------------------------------------------
    // M2 — the traced mailbox
    // -----------------------------------------------------------------

    #[test]
    fn a_message_travels_both_ways_and_is_consumed_once() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());

        run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                from: Some("claude-sid".to_owned()),
                message: "checkpoint published".to_owned(),
                expires_in: None,
            },
        )
        .unwrap();
        run_send(
            &ctx,
            &SendArgs {
                to: "claude-sid".to_owned(),
                from: Some("codex-sid".to_owned()),
                message: "your evidence ref is circular".to_owned(),
                expires_in: None,
            },
        )
        .unwrap();

        let mb = PilotMailbox::new(dir.path());
        let now = Utc::now();
        assert_eq!(
            mb.pending(&SessionId::new("codex-sid").unwrap(), now)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            mb.pending(&SessionId::new("claude-sid").unwrap(), now)
                .unwrap()
                .len(),
            1
        );

        // Codex reads its inbox — and only its own.
        run_inbox(
            &ctx,
            &InboxArgs {
                session: Some("codex-sid".to_owned()),
                ..InboxArgs::default()
            },
        )
        .unwrap();
        assert!(mb
            .pending(&SessionId::new("codex-sid").unwrap(), now)
            .unwrap()
            .is_empty());
        assert_eq!(
            mb.pending(&SessionId::new("claude-sid").unwrap(), now)
                .unwrap()
                .len(),
            1,
            "reading one mailbox does not drain the other",
        );
    }

    #[test]
    fn peek_shows_without_consuming() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                from: Some("claude-sid".to_owned()),
                message: "look but do not take".to_owned(),
                expires_in: None,
            },
        )
        .unwrap();

        run_inbox(
            &ctx,
            &InboxArgs {
                session: Some("codex-sid".to_owned()),
                peek: true,
                ..InboxArgs::default()
            },
        )
        .unwrap();

        let mb = PilotMailbox::new(dir.path());
        assert_eq!(
            mb.pending(&SessionId::new("codex-sid").unwrap(), Utc::now())
                .unwrap()
                .len(),
            1,
            "a peek acknowledges nothing",
        );
    }

    #[test]
    fn the_body_round_trips_through_the_content_store() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let body = "a body long enough that inlining it would bloat the registry line";
        run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                from: Some("claude-sid".to_owned()),
                message: body.to_owned(),
                expires_in: None,
            },
        )
        .unwrap();

        let mb = PilotMailbox::new(dir.path());
        let pending = mb
            .pending(&SessionId::new("codex-sid").unwrap(), Utc::now())
            .unwrap();
        let cas = FileCas::new(dir.path().join("cas"));
        let hash = ContentHash::new(pending[0].message.payload_hash.clone()).unwrap();
        assert_eq!(String::from_utf8(cas.get(&hash).unwrap()).unwrap(), body);

        // The envelope itself carries no body — only a reference to one.
        let line =
            fs::read_to_string(mb.inbox_path(&SessionId::new("codex-sid").unwrap())).unwrap();
        assert!(
            !line.contains(body),
            "the registry line holds envelopes only"
        );
    }

    #[test]
    fn an_expired_message_is_shown_rather_than_dropped() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                from: Some("claude-sid".to_owned()),
                message: "take over now".to_owned(),
                // Already expired by the time anyone reads it.
                expires_in: Some(-1),
            },
        )
        .unwrap();

        let mb = PilotMailbox::new(dir.path());
        let pending = mb
            .pending(&SessionId::new("codex-sid").unwrap(), Utc::now())
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].state.as_str(),
            "expired",
            "a stale instruction must not read like a fresh one",
        );

        // It still renders — and reading it still consumes it, so the
        // operator is not shown the same dead instruction forever.
        run_inbox(
            &ctx,
            &InboxArgs {
                session: Some("codex-sid".to_owned()),
                ..InboxArgs::default()
            },
        )
        .unwrap();
        assert!(mb
            .pending(&SessionId::new("codex-sid").unwrap(), Utc::now())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_send_without_a_sender_is_refused() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        // The env may or may not carry COSMON_SESSION_ID in a worker; only
        // assert the refusal when it genuinely is absent.
        if std::env::var("COSMON_SESSION_ID").is_ok() || std::env::var("CLAUDE_SESSION_ID").is_ok()
        {
            return;
        }
        let err = run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                message: "from whom?".to_owned(),
                ..SendArgs::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no sender"), "{err}");
    }

    // ADR-168 §D3.6 and the M2 acceptance clause: the mailbox is a *third*
    // channel. It must not appear in, drain, or otherwise disturb the
    // `--to-session` text log — the two live in different files and neither
    // reader sees the other's traffic.
    #[test]
    fn the_envelope_channel_and_the_text_channel_do_not_touch() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let sid = SessionId::new("codex-sid").unwrap();

        super::super::whisper::run_to_session(&ctx, "codex-sid", b"a plain whisper", false)
            .unwrap();
        run_send(
            &ctx,
            &SendArgs {
                to: "codex-sid".to_owned(),
                from: Some("claude-sid".to_owned()),
                message: "a traced envelope".to_owned(),
                expires_in: None,
            },
        )
        .unwrap();

        let store = PresenceStore::new(dir.path());
        let log = fs::read_to_string(store.log_path(&sid)).unwrap();
        assert!(log.contains("a plain whisper"));
        assert!(
            !log.contains("a traced envelope"),
            "the envelope must not leak into the text channel: {log}",
        );

        // Draining the mailbox leaves the text channel's seek untouched…
        run_inbox(
            &ctx,
            &InboxArgs {
                session: Some("codex-sid".to_owned()),
                ..InboxArgs::default()
            },
        )
        .unwrap();
        assert!(
            !store.seek_path(&sid).exists(),
            "inbox does not move the seek"
        );

        // …and polling the text channel leaves no ack behind.
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("codex-sid".to_owned()),
            },
        )
        .unwrap();
        assert!(store.seek_path(&sid).exists());
        let mb = PilotMailbox::new(dir.path());
        assert_eq!(
            mb.acks(&sid).unwrap().len(),
            1,
            "poll added no acknowledgement of its own",
        );
    }

    // -----------------------------------------------------------------
    // ADR-168 §D4 — the byte-cursor channel's two silent failures
    // -----------------------------------------------------------------

    // P4: a seek past the end means the log rotated. Serving the backlog is
    // the only reading that does not lose a message with a success exit code.
    #[test]
    fn a_rotated_log_serves_its_backlog_instead_of_swallowing_it() {
        let content = "fresh line after rotation\n";
        assert_eq!(clamp_seek(9_999, content), 0);
        // Equal is "nothing new", which is a different fact.
        assert_eq!(clamp_seek(content.len(), content), content.len());
    }

    // P5: a seek landing inside a multi-byte character used to panic the
    // reader on the `&content[seek..]` slice.
    #[test]
    fn a_seek_inside_a_character_does_not_panic_the_reader() {
        let content = "héllo\n"; // 'é' is two bytes at offsets 1..3.
        assert!(!content.is_char_boundary(2));
        let at = clamp_seek(2, content);
        assert_eq!(at, 1, "walk back to the nearest boundary");
        // The real property: slicing there is legal.
        let _ = &content[at..];
    }

    #[test]
    fn poll_survives_a_seek_wedged_in_a_multi_byte_character() {
        let dir = tempdir().unwrap();
        let presence = dir.path().join("presence");
        fs::create_dir_all(&presence).unwrap();
        fs::write(presence.join("session-utf8.log"), "héllo\n").unwrap();
        fs::write(presence.join("session-utf8.seek"), "2").unwrap();

        let ctx = ctx_for(dir.path());
        run_poll(
            &ctx,
            &PollArgs {
                session: Some("session-utf8".to_owned()),
            },
        )
        .unwrap();
    }
}
