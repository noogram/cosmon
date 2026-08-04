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
//! Seven subcommands ship together:
//!
//! - `ping` — upsert this session's snapshot (C-PRESENCE-CORE).
//! - `ls` — scan the directory and render live peers.
//! - `gc` — sweep stale snapshots whose pids no longer exist.
//! - `poll` — pull new whisper log lines since the last read.
//! - `send` — deliver one traced envelope to a peer's mailbox (M2).
//! - `inbox` — read and acknowledge this session's pending envelopes (M2).
//! - `lease` — inspect, request and grant the PRIMARY lease (M4).
//!
//! # The seat is a claim; the lease is the fact
//!
//! `ping --role primary` used to be a self-declaration anyone could make. It
//! is now checked against the mission's lease ledger before the snapshot is
//! written, so the registry cannot show two primaries even for an instant
//! (ADR-168 §D6). A pilot whose lease has been transferred away is demoted by
//! its own next heartbeat rather than failing it — the session is still alive,
//! and a heartbeat that errored would blind the fleet to that fact.
//!
//! Composition: the first six share a single `PresenceStore` pointed at
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
use cosmon_core::operator_attestation::{
    GrantChallenge, OperatorAttestation, OperatorGestureVerifier, OperatorKeyId,
};
use cosmon_core::pilot_lease::{
    LeaseDecision, LeaseEpoch, LeaseRequest, PilotLease, RefusalReason, RequestId,
};
use cosmon_core::pilot_message::PilotMessage;
use cosmon_core::presence::{PilotRole, Presence};
use cosmon_filestore::cas::FileCas;
use cosmon_filestore::{
    MinisignOperatorVerifier, PilotLeaseStore, PilotMailbox, PresenceStore, TAKEOVER_PUBKEY_ENV,
    TAKEOVER_PUBKEY_REL,
};
use cosmon_notary::minisign::MinisignSignature;

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
    /// Inspect, request and grant the PRIMARY lease on a mission.
    #[command(subcommand)]
    Lease(LeaseSub),
}

/// `cs presence lease <sub>` — the authority surface of ADR-168 §D6.
///
/// Four verbs and deliberately no fifth. There is no `takeover`, no `steal`
/// and no `auto`: a transfer is `request` (a pilot asks, and gains nothing)
/// followed by `grant` (the operator decides). Quota-triggered takeover is
/// refused by the ADR, and the way to keep it refused is for the code to have
/// nowhere to put it.
#[derive(clap::Subcommand)]
pub enum LeaseSub {
    /// Show who holds a mission's controls, at which epoch, and what has been
    /// asked.
    Show(LeaseShowArgs),
    /// Ask for the controls. Writes a request and confers no authority.
    Request(LeaseRequestArgs),
    /// Operator gesture: hand the controls to a session at the next epoch.
    Grant(LeaseGrantArgs),
    /// Ask the guard whether a session may pilot, and exit 0 or 1 accordingly.
    Check(LeaseCheckArgs),
}

/// Arguments for `cs presence lease show`.
#[derive(clap::Args)]
pub struct LeaseShowArgs {
    /// Mission whose lease to inspect.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Print every grant ever recorded, oldest first, instead of only the head.
    #[arg(long)]
    pub history: bool,
}

/// Arguments for `cs presence lease request`.
#[derive(clap::Args)]
pub struct LeaseRequestArgs {
    /// Mission the controls are being asked for.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Session that would become PRIMARY. Defaults to the requester.
    #[arg(long, value_name = "SID")]
    pub to: Option<String>,
    /// Session doing the asking. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub from: Option<String>,
    /// One line the operator reads before deciding.
    #[arg(long, value_name = "TEXT", default_value = "")]
    pub reason: String,
}

/// Arguments for `cs presence lease grant`.
#[derive(clap::Args)]
pub struct LeaseGrantArgs {
    /// Mission whose controls are being handed over.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Request being answered. The holder is taken from the request.
    #[arg(long, value_name = "REQUEST_ID")]
    pub request: Option<String>,
    /// Session to seat, when granting without a request.
    #[arg(long, value_name = "SID")]
    pub to: Option<String>,
    /// Seconds after which the lease authorises nothing. Omitted means it
    /// holds until the next grant supersedes it.
    #[arg(long = "ttl", value_name = "SECONDS")]
    pub ttl: Option<i64>,
    /// Operator identity to record. Defaults to `$USER`. Covered by the
    /// attestation, so it is a signed claim rather than a free string.
    #[arg(long = "by", value_name = "NAME")]
    pub granted_by: Option<String>,
    /// Path to the operator's detached minisign signature over the challenge
    /// this grant produces, or `-` to read it from stdin. Without it the
    /// grant is refused: `--by` names an operator, it does not attest one.
    #[arg(long, value_name = "PATH")]
    pub attestation: Option<PathBuf>,
}

/// Arguments for `cs presence lease check`.
#[derive(clap::Args)]
pub struct LeaseCheckArgs {
    /// Mission the gesture would touch.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Session issuing the gesture. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// The epoch the caller believes it holds. Omitting it is itself a
    /// refusal — a gesture must name the generation it was written against.
    #[arg(long, value_name = "N")]
    pub epoch: Option<u64>,
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
    /// Mission this pilot's seat is about. Required to take the `primary`
    /// seat, because authority is per-mission.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: Option<MoleculeId>,
    /// The lease epoch this pilot believes it holds. Required to take the
    /// `primary` seat: the guard checks a claim, and a claim that names no
    /// epoch is not one.
    #[arg(long, value_name = "N")]
    pub epoch: Option<u64>,
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
        Sub::Lease(a) => run_lease(ctx, a),
    }
}

/// The cosmon state root these registries live under.
///
/// `pub(crate)` because `cs sessions` (M5) composes the same stores rather
/// than re-deriving where they are. One writer of a path, many readers.
pub(crate) fn state_root(ctx: &Context) -> PathBuf {
    ctx.config.clone().unwrap_or_else(super::default_state_dir)
}

pub(crate) fn store(ctx: &Context) -> PresenceStore {
    PresenceStore::new(state_root(ctx))
}

pub(crate) fn mailbox(ctx: &Context) -> PilotMailbox {
    PilotMailbox::new(state_root(ctx))
}

/// Parse the `--role` token into a [`PilotRole`].
///
/// An unrecognised token is an **error**, not a fallback to `copilot`. A
/// silent fallback would turn `--role primry` into a demotion the operator
/// never sees; fail-closed means refusing the gesture, not guessing at it.
pub(crate) fn parse_role(raw: &str) -> anyhow::Result<PilotRole> {
    match raw {
        "primary" => Ok(PilotRole::Primary),
        "copilot" => Ok(PilotRole::Copilot),
        other => Err(anyhow::anyhow!(
            "unknown role '{other}' — expected 'primary' or 'copilot'"
        )),
    }
}

/// The seat a ping will actually write, after the guard has had its say.
struct Seat {
    /// The role that goes on disk — never a primary the ledger would refuse.
    role: PilotRole,
    /// Mission the seat is about, carried forward when the ping omits it.
    mission: Option<MoleculeId>,
    /// Epoch claimed on that mission. Cleared when the claim was refused, so
    /// the snapshot does not advertise a generation it does not hold.
    lease_epoch: Option<LeaseEpoch>,
    /// Why the primary claim was refused, when it was.
    refusal: Option<String>,
}

/// Decide the seat for this ping: what the operator asked for, what the last
/// ping left behind, and what the lease ledger actually permits.
///
/// The whole point of doing this before the write is ADR-168 §D6 — a refused
/// gesture is refused *before* it takes effect. A presence file that said
/// `primary` and was corrected afterwards would have been true for a moment,
/// and a peer scanning during that moment would have read a second primary.
fn resolve_seat(
    ctx: &Context,
    args: &PingArgs,
    session_id: &SessionId,
    prior: Option<&Presence>,
    now: DateTime<Utc>,
) -> anyhow::Result<Seat> {
    let claimed_role = match &args.role {
        Some(raw) => parse_role(raw)?,
        None => prior.map_or_else(PilotRole::default, |p| p.role),
    };
    let mission = args
        .mission
        .clone()
        .or_else(|| prior.and_then(|p| p.mission.clone()));
    let lease_epoch = match args.epoch {
        Some(raw) => Some(LeaseEpoch::new(raw)?),
        None => prior.and_then(|p| p.lease_epoch),
    };

    let refusal = if claimed_role.is_primary() {
        primary_claim_refusal(ctx, session_id, mission.as_ref(), lease_epoch, now)?
    } else {
        None
    };
    let Some(why) = refusal else {
        return Ok(Seat {
            role: claimed_role,
            mission,
            lease_epoch,
            refusal: None,
        });
    };

    // An explicit `--role primary` is a gesture, and a refused gesture fails.
    // A *carried-forward* primary is a heartbeat, and failing a heartbeat
    // would blind the fleet to a session that is very much alive — so it is
    // demoted, visibly, and the ping still lands.
    if args.role.is_some() {
        return Err(anyhow::anyhow!(
            "refusing the primary seat for {sid}: {why}",
            sid = session_id.as_str(),
        ));
    }
    Ok(Seat {
        role: PilotRole::Copilot,
        mission,
        lease_epoch: None,
        refusal: Some(why),
    })
}

fn run_ping(ctx: &Context, args: &PingArgs) -> anyhow::Result<()> {
    let presence = ping(ctx, args)?;

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

/// Write this session's presence snapshot and return what actually landed.
///
/// The seat is resolved against the lease ledger *before* the write, so the
/// returned [`Presence`] is the truth on disk and not the claim that was made
/// — a caller rendering it cannot accidentally announce a primary the ledger
/// refused. The demotion notice goes to stderr here rather than at a
/// call-site, because every surface that pings owes the pilot that sentence.
///
/// # Errors
///
/// Propagates id, role and epoch parse failures, an explicitly-claimed primary
/// seat the ledger refuses, and filesystem errors from the presence store.
pub(crate) fn ping(ctx: &Context, args: &PingArgs) -> anyhow::Result<Presence> {
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

    let seat = resolve_seat(ctx, args, &session_id, prior.as_ref(), now)?;
    let Seat {
        role,
        mission,
        lease_epoch,
        refusal,
    } = seat;
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
        mission,
        lease_epoch,
        ..Presence::new(
            session_id.clone(),
            args.galaxy.clone(),
            cwd,
            std::process::id(),
            prior_started_at.unwrap_or(now),
        )
    };
    store.upsert(&presence)?;

    if let Some(why) = &refusal {
        // Not an error — the heartbeat succeeded. But a pilot that believed it
        // was flying has to be told it is not, on stderr so a `--json` consumer
        // still parses one object on stdout.
        eprintln!(
            "presence ping: demoted {sid} to copilot — {why}",
            sid = presence.session_id.as_str(),
        );
    }
    Ok(presence)
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
    let (message, written) = deliver(ctx, args)?;
    let to = &message.to;

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

/// Content-address the body, envelope it, and append it to the destination's
/// mailbox. Returns the envelope and whether this call is the one that wrote
/// it — a redelivery of an identical envelope is a no-op, which is what makes
/// consumption idempotent (MESSAGE-TRACE).
///
/// # Errors
///
/// Propagates id parse failures, a missing sender identity, and filesystem
/// errors from the CAS or the mailbox.
pub(crate) fn deliver(ctx: &Context, args: &SendArgs) -> anyhow::Result<(PilotMessage, bool)> {
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
    Ok((message, written))
}

/// One envelope as a reader sees it: the mailbox entry and its body, when the
/// body could be loaded from the CAS.
pub(crate) type RenderedMessage = (cosmon_filestore::MailboxEntry, Option<String>);

/// Read a session's mailbox and resolve every body, **without** acknowledging
/// anything.
///
/// Reading and acknowledging are two calls on purpose: an ack claims the
/// reader has the text, so it may only happen after the text has left the
/// process. Every surface that consumes this channel owes the same ordering,
/// and splitting the pair is what lets `cs sessions inbox` inherit it instead
/// of re-deriving it (ADR-168 §D4, probe P3).
///
/// # Errors
///
/// Propagates a missing session identity and filesystem errors from the
/// mailbox.
pub(crate) fn collect_inbox(
    ctx: &Context,
    args: &InboxArgs,
) -> anyhow::Result<(SessionId, Vec<RenderedMessage>)> {
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
    let mut rendered: Vec<RenderedMessage> = Vec::with_capacity(entries.len());
    for e in entries {
        let body = cas
            .get(&ContentHash::new(e.message.payload_hash.clone())?)
            .ok()
            .and_then(|b| String::from_utf8(b).ok());
        rendered.push((e, body));
    }
    Ok((sid, rendered))
}

/// Acknowledge every envelope whose body the caller has actually emitted.
///
/// An envelope whose payload could not be read is left pending: leaving it to
/// be redelivered is the honest state, and at-least-once is the guarantee
/// this channel makes.
///
/// # Errors
///
/// Propagates filesystem errors from the mailbox's ack ledger.
pub(crate) fn ack_consumed(
    ctx: &Context,
    sid: &SessionId,
    rendered: &[RenderedMessage],
) -> anyhow::Result<()> {
    let mailbox = mailbox(ctx);
    let now = Utc::now();
    for (e, body) in rendered {
        if body.is_none() {
            continue;
        }
        if e.read_at.is_none() {
            mailbox.ack(sid, &e.message.id, now)?;
        }
    }
    Ok(())
}

fn run_inbox(ctx: &Context, args: &InboxArgs) -> anyhow::Result<()> {
    let (sid, rendered) = collect_inbox(ctx, args)?;

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
        ack_consumed(ctx, &sid, &rendered)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The PRIMARY lease — authority, and its supervised transfer (M4)
// ---------------------------------------------------------------------------

/// The lease ledger, with the galaxy's pinned operator key attached.
///
/// The key is resolved here rather than at each call site so that no caller
/// can *forget* it: a store built without a trust root honours no grant, and
/// a code path that forgot to pin one would look like a mission nobody has
/// ever granted rather than like a bug. See [`MinisignOperatorVerifier`].
///
/// # Errors
///
/// Propagates an unreadable or malformed key file. A galaxy with **no** key
/// pinned is not an error here: it is a store that refuses every grant, and
/// `cs sessions takeover trust` is where that fact is reported.
pub(crate) fn leases(ctx: &Context) -> anyhow::Result<PilotLeaseStore> {
    leases_at(&state_root(ctx))
}

/// The lease ledger over an explicit state root, trust root attached.
///
/// The same store [`leases`] builds, for the one caller that has a path rather
/// than a [`Context`]: the authority guard on the lifecycle verbs
/// (`super::guard::refuse_unleased_pilot_gesture`). It exists because the
/// guard once built `PilotLeaseStore::new` directly, and a store with no
/// pinned key honours no grant — so every leased mission read back as
/// *unleased* and the guard returned `Ok(())` for callers the ledger refused.
/// The M8 relève exercise caught it by collapsing a leased mission from an
/// unleased co-pilot while `cs sessions takeover check`, reading the same
/// ledger through [`leases`], refused the very same session.
///
/// Both readers now come through here, which is the point: this is one
/// function so that "resolve the trust root" is not a step a call site can
/// perform differently, or forget.
///
/// # Errors
///
/// As [`leases`].
pub(crate) fn leases_at(state_root: &std::path::Path) -> anyhow::Result<PilotLeaseStore> {
    let store = PilotLeaseStore::new(state_root);
    Ok(
        match MinisignOperatorVerifier::resolve_for_state_root(state_root)? {
            Some(v) => store.trusting(std::sync::Arc::new(v)),
            None => store,
        },
    )
}

/// Why `session`'s claim to the primary seat on `mission` at `epoch` would be
/// refused — `None` when it holds up.
///
/// A claim missing either half is refused here rather than passed to the
/// guard, because the guard answers "may this session act on this mission",
/// and a claim with no mission has not asked a question it could answer.
fn primary_claim_refusal(
    ctx: &Context,
    session: &SessionId,
    mission: Option<&MoleculeId>,
    epoch: Option<LeaseEpoch>,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<String>> {
    let Some(mission) = mission else {
        return Ok(Some(
            "the primary seat needs --mission: authority is per-mission, and a \
             seat that names none backs nothing"
                .to_owned(),
        ));
    };
    let decision = leases(ctx)?.authorize(mission, now, session, epoch)?;
    Ok(decision.refusal().map(RefusalReason::explain))
}

fn run_lease(ctx: &Context, sub: &LeaseSub) -> anyhow::Result<()> {
    match sub {
        LeaseSub::Show(a) => run_lease_show(ctx, a),
        LeaseSub::Request(a) => run_lease_request(ctx, a),
        LeaseSub::Grant(a) => run_lease_grant(ctx, a),
        LeaseSub::Check(a) => run_lease_check(ctx, a),
    }
}

fn run_lease_show(ctx: &Context, args: &LeaseShowArgs) -> anyhow::Result<()> {
    let store = leases(ctx)?;
    let now = Utc::now();
    let current = store.current(&args.mission)?;
    let pending = store.unanswered_requests(&args.mission)?;

    if ctx.json {
        let v = serde_json::json!({
            "mission": args.mission.as_str(),
            "lease": current,
            "valid_now": current.as_ref().is_some_and(|l| l.is_valid_at(now)),
            "next_epoch": store.next_epoch(&args.mission)?,
            "unanswered_requests": pending,
            "history": if args.history { Some(store.grants(&args.mission)?) } else { None },
        });
        println!("{v}");
        return Ok(());
    }

    match &current {
        None => println!(
            "lease {mission}: none — nobody is PRIMARY, so nobody may pilot",
            mission = args.mission.as_str(),
        ),
        Some(l) => println!(
            "lease {mission}: {holder} at epoch {epoch}{validity} — granted by {by} at {at}",
            mission = args.mission.as_str(),
            holder = l.holder_session_id.as_str(),
            epoch = l.epoch,
            validity = if l.is_valid_at(now) {
                String::new()
            } else {
                " (EXPIRED)".to_owned()
            },
            by = l.granted_by,
            at = l.granted_at,
        ),
    }
    if pending.is_empty() {
        println!("  (no unanswered requests)");
    } else {
        for r in &pending {
            println!(
                "  request {id}: {who} asks for the controls{reason}",
                id = r.id,
                who = r.candidate_session_id.as_str(),
                reason = if r.reason.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", r.reason)
                },
            );
        }
    }
    if args.history {
        for l in store.grants(&args.mission)? {
            println!(
                "  epoch {epoch}: {holder} (by {by} at {at})",
                epoch = l.epoch,
                holder = l.holder_session_id.as_str(),
                by = l.granted_by,
                at = l.granted_at,
            );
        }
    }
    Ok(())
}

fn run_lease_request(ctx: &Context, args: &LeaseRequestArgs) -> anyhow::Result<()> {
    let (request, written) = request_lease(ctx, args)?;

    if ctx.json {
        let v = serde_json::json!({
            "request": request,
            "recorded": written,
            "authority_changed": false,
        });
        println!("{v}");
    } else if written {
        println!(
            "lease request {id} recorded — it confers nothing until an operator runs \
             `cs presence lease grant --mission {mission} --request {id}`",
            id = request.id,
            mission = args.mission.as_str(),
        );
    } else {
        println!(
            "lease request {id} was already recorded — asking twice is asking once",
            id = request.id,
        );
    }
    Ok(())
}

/// Record a pilot's ask for the controls. Returns the request and whether this
/// call is the one that wrote it.
///
/// This confers no authority whatsoever, by construction: it appends to the
/// requests file, and the authority ledger is a different file only an
/// operator's grant appends to (ADR-168 §D5).
///
/// # Errors
///
/// Propagates id parse failures, a missing requester identity, and filesystem
/// errors from the lease store.
pub(crate) fn request_lease(
    ctx: &Context,
    args: &LeaseRequestArgs,
) -> anyhow::Result<(LeaseRequest, bool)> {
    let requester = match &args.from {
        Some(s) => SessionId::new(s.clone())?,
        None => resolve_sid_for_poll(&PollArgs { session: None }).map_err(|_| {
            anyhow::anyhow!("no requester — pass --from <SID> or export COSMON_SESSION_ID")
        })?,
    };
    let candidate = match &args.to {
        Some(s) => SessionId::new(s.clone())?,
        None => requester.clone(),
    };
    let store = leases(ctx)?;
    let observed = store.current(&args.mission)?;
    let request = LeaseRequest::new(
        args.mission.clone(),
        candidate,
        requester,
        observed.as_ref(),
        Utc::now(),
        args.reason.clone(),
    );
    let written = store.request(&request)?;
    Ok((request, written))
}

fn run_lease_grant(ctx: &Context, args: &LeaseGrantArgs) -> anyhow::Result<()> {
    let lease = grant_lease(ctx, args)?;

    if ctx.json {
        println!("{}", serde_json::to_string(&lease)?);
    } else {
        println!(
            "lease {mission}: {holder} is PRIMARY at epoch {epoch} — every earlier epoch \
             is now refused",
            mission = args.mission.as_str(),
            holder = lease.holder_session_id.as_str(),
            epoch = lease.epoch,
        );
    }
    Ok(())
}

/// The operator gesture: seat a session as PRIMARY at the next epoch.
///
/// One append to the grants ledger, which is what makes a transfer atomic —
/// there is no half-transferred state a crash could leave behind.
///
/// # Errors
///
/// Propagates an unknown request id, a grant naming neither a request nor a
/// session, and filesystem errors from the lease store.
pub(crate) fn grant_lease(ctx: &Context, args: &LeaseGrantArgs) -> anyhow::Result<PilotLease> {
    let store = leases(ctx)?;
    let now = Utc::now();
    let trusted = store.trusted_key_id().ok_or_else(|| {
        anyhow::anyhow!(
            "no operator public key pinned — nothing here could tell your gesture from \
             an agent imitating it, so no grant is written. Pin one at \
             `<galaxy>/{TAKEOVER_PUBKEY_REL}` (or set ${TAKEOVER_PUBKEY_ENV}); \
             `minisign -G` makes the pair."
        )
    })?;

    // The holder comes from the request when there is one, so the operator
    // grants *what was asked for* rather than retyping it beside the id.
    let (holder, answered) = match (&args.request, &args.to) {
        (Some(raw), _) => {
            let id = RequestId::new(raw.clone())?;
            let found = store.find_request(&args.mission, &id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "no request {id} on mission {mission} — `cs presence lease show \
                     --mission {mission}` lists the ones there are",
                    mission = args.mission.as_str(),
                )
            })?;
            (found.candidate_session_id.clone(), Some(found))
        }
        (None, Some(sid)) => (SessionId::new(sid.clone())?, None),
        (None, None) => {
            return Err(anyhow::anyhow!(
                "nothing to grant — pass --request <REQUEST_ID> to answer an ask, \
                 or --to <SID> to seat a session directly"
            ))
        }
    };

    let epoch = store.next_epoch(&args.mission)?;
    let granted_by = resolve_operator_name(args.granted_by.as_deref());
    let challenge = GrantChallenge::new(
        args.mission.clone(),
        holder.clone(),
        epoch,
        granted_by.clone(),
        args.ttl,
    )?;

    let attestation = read_attestation(args.attestation.as_deref(), &challenge)?;

    // Checked here as well as on every read. The read-time check in
    // `PilotLeaseStore::grants` is the mechanism — it is what refuses a line
    // an agent appended without going through this command at all. This one
    // is ergonomics: it turns "your grant is silently inert" into a message
    // naming which field of the challenge the signature does not cover.
    MinisignOperatorVerifier::resolve_for_state_root(state_root(ctx))?
        .ok_or_else(|| anyhow::anyhow!("no operator public key pinned"))?
        .verify(&challenge, &attestation)
        .map_err(|e| {
            anyhow::anyhow!(
                "the attestation does not authorise this transfer: {e}\n\
                 the challenge cosmon computed was:\n{challenge}\n\
                 pinned operator key: {trusted}"
            )
        })?;

    let mut lease = PilotLease::new(
        args.mission.clone(),
        holder,
        epoch,
        granted_by,
        now,
        args.ttl.map(|secs| now + chrono::Duration::seconds(secs)),
    )
    .attested_by(attestation);
    if let Some(r) = &answered {
        lease = lease.answering(r);
    }
    store.grant(&lease)?;
    Ok(lease)
}

/// The operator name a grant claims: `--by`, else `$USER`, else a placeholder.
///
/// Shared with `cs sessions takeover challenge` so the bytes the operator
/// signs and the bytes cosmon later checks are produced by the same rule — a
/// second resolution order would make the two disagree exactly when `--by` is
/// omitted, which is the common case.
pub(crate) fn resolve_operator_name(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown-operator".to_owned())
}

/// Read a detached minisign signature from a path, or from stdin for `-`.
fn read_attestation(
    path: Option<&Path>,
    challenge: &GrantChallenge,
) -> anyhow::Result<OperatorAttestation> {
    let path = path.ok_or_else(|| {
        anyhow::anyhow!(
            "this grant carries no operator attestation, so it would seat nobody.\n\
             The gesture is a signature, not a flag — `--by` is a label an agent can \
             type as easily as you can.\n\
             \n\
             Sign the transfer, then pass the signature:\n\
             \n  \
             cs sessions takeover challenge --mission {mission} --to {holder} \
             --by {by} > takeover.txt\n  \
             minisign -Sm takeover.txt          # your passphrase — the part no agent has\n  \
             cs sessions takeover grant --mission {mission} --to {holder} --by {by} \
             --attestation takeover.txt.minisig",
            mission = challenge.mission_id.as_str(),
            holder = challenge.holder_session_id.as_str(),
            by = challenge.granted_by,
        )
    })?;

    let text = if path == Path::new("-") {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .map_err(|e| anyhow::anyhow!("failed to read the attestation from stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?
    };

    let parsed = MinisignSignature::parse(&text)
        .map_err(|e| anyhow::anyhow!("{} is not a minisign signature: {e}", path.display()))?;
    Ok(OperatorAttestation {
        key_id: OperatorKeyId::from_bytes(parsed.key_id),
        signature: parsed.signature_line(),
        global_signature: parsed.global_signature_line(),
        trusted_comment: parsed.trusted_comment.clone(),
        untrusted_comment: parsed.untrusted_comment.clone(),
    })
}

/// The guard, exposed as a verb. Exits 0 when the gesture may proceed and 1
/// when it may not — so a shell script and the Rust call-site enforce the same
/// rule instead of two rules that agree today.
fn run_lease_check(ctx: &Context, args: &LeaseCheckArgs) -> anyhow::Result<()> {
    let session = match &args.session {
        Some(s) => SessionId::new(s.clone())?,
        None => resolve_sid_for_poll(&PollArgs { session: None }).map_err(|_| {
            anyhow::anyhow!("no session — pass --session <SID> or export COSMON_SESSION_ID")
        })?,
    };
    let epoch = match args.epoch {
        Some(raw) => Some(LeaseEpoch::new(raw)?),
        None => None,
    };
    let decision = leases(ctx)?.authorize(&args.mission, Utc::now(), &session, epoch)?;

    if ctx.json {
        println!("{}", serde_json::to_string(&decision)?);
    } else {
        match &decision {
            LeaseDecision::Granted { epoch } => println!(
                "granted: {sid} holds {mission} at epoch {epoch}",
                sid = session.as_str(),
                mission = args.mission.as_str(),
            ),
            LeaseDecision::Refused(reason) => println!(
                "refused: {sid} may not pilot {mission} — {why}",
                sid = session.as_str(),
                mission = args.mission.as_str(),
                why = reason.explain(),
            ),
        }
    }
    std::io::stdout().flush().ok();
    // Same shape as `cs diverge`: a decidable answer is a successful run with
    // a non-zero exit code, not an error.
    std::process::exit(i32::from(!decision.is_granted()));
}

// ---------------------------------------------------------------------------
// Session id resolution
// ---------------------------------------------------------------------------

/// Resolve a session id for `ping`: explicit `--session` wins, then
/// `$COSMON_SESSION_ID`, then `$CLAUDE_SESSION_ID`, then a stable
/// tty-hash fallback so two shells in different tabs get distinct ids.
pub(crate) fn resolve_or_derive_sid(explicit: Option<&str>) -> anyhow::Result<SessionId> {
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
pub(crate) fn resolve_sid_for_poll(args: &PollArgs) -> anyhow::Result<SessionId> {
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

pub(crate) fn format_age(now: DateTime<Utc>, heartbeat: DateTime<Utc>) -> String {
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
    use cosmon_minisign_testkit::Operator;
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

    /// Grant `sid` the lease and return the flags a `primary` ping now needs.
    ///
    /// M4 made the primary seat lease-backed: `--role primary` is a claim the
    /// guard checks, so a test that wants a primary has to say who granted it.
    /// These M2 tests are about presence, not authority — the helper keeps
    /// their subject unchanged.
    fn seated_as_primary(
        ctx: &Context,
        state: &Path,
        sid: &str,
    ) -> (Option<MoleculeId>, Option<u64>) {
        let epoch = grant_to(ctx, state, sid);
        (Some(mission()), Some(epoch.get()))
    }

    /// The acceptance clause, in one test: Claude sees Codex and Codex sees
    /// Claude, each knowing which seat the other holds and who is following
    /// whom — from a directory scan, with no broker anywhere.
    #[test]
    fn claude_sees_codex_and_codex_sees_claude() {
        let (dir, ctx, state) = lease_world();

        let (mission, epoch) = seated_as_primary(&ctx, &state, "claude-sid");
        ping(
            &ctx,
            PingArgs {
                session: Some("claude-sid".to_owned()),
                galaxy: "stagecraft".to_owned(),
                provider: Some("claude".to_owned()),
                native_session_id: Some("4940f28e".to_owned()),
                role: Some("primary".to_owned()),
                mission,
                epoch,
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

        let seen = PresenceStore::new(&state).scan().unwrap();
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
        let (dir, ctx, state) = lease_world();
        let (mission, epoch) = seated_as_primary(&ctx, &state, "pilot");
        ping(
            &ctx,
            PingArgs {
                session: Some("pilot".to_owned()),
                role: Some("primary".to_owned()),
                provider: Some("claude".to_owned()),
                native_session_id: Some("abc".to_owned()),
                mission,
                epoch,
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

        let p = PresenceStore::new(&state).scan().unwrap()[0].clone();
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
        let (_dir, ctx, state) = lease_world();
        let (mission, epoch) = seated_as_primary(&ctx, &state, "primary-sid");
        ping(
            &ctx,
            PingArgs {
                session: Some("primary-sid".to_owned()),
                role: Some("primary".to_owned()),
                mission,
                epoch,
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

    // -----------------------------------------------------------------------
    // The PRIMARY lease, at the operator surface (M4)
    // -----------------------------------------------------------------------

    fn mission() -> MoleculeId {
        MoleculeId::new("task-20260731-9cf4").unwrap()
    }

    /// The human at the keyboard. Since ADR-171 a grant is that human's
    /// signature, so a lease test needs one; `cosmon-minisign-testkit` holds a
    /// secret key the shipped tree has no way to use.
    fn operator() -> &'static Operator {
        static OP: std::sync::OnceLock<Operator> = std::sync::OnceLock::new();
        OP.get_or_init(|| Operator::from_seed(23))
    }

    /// A galaxy laid out the way `leases` expects — `<root>/.cosmon/state` for
    /// the ledger and `<root>/.cosmon/takeover.pub` for the trust root — so
    /// these tests resolve the key by the real rule instead of an override.
    fn lease_world() -> (tempfile::TempDir, Context, PathBuf) {
        let dir = tempdir().unwrap();
        let cosmon = dir.path().join(".cosmon");
        let state = cosmon.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(cosmon.join("takeover.pub"), operator().public_key_file()).unwrap();
        let ctx = ctx_for(&state);
        (dir, ctx, state)
    }

    /// Sign the transfer that would land next, and return the signature path.
    fn sign_grant(ctx: &Context, state: &Path, holder: &str, ttl: Option<i64>) -> PathBuf {
        let epoch = leases(ctx).unwrap().next_epoch(&mission()).unwrap();
        let challenge = GrantChallenge::new(
            mission(),
            SessionId::new(holder.to_owned()).unwrap(),
            epoch,
            "test-operator",
            ttl,
        )
        .unwrap();
        let path = state.join(format!("attest-{epoch}-{holder}.minisig"));
        fs::write(&path, operator().sign(&challenge.canonical_bytes())).unwrap();
        path
    }

    fn grant_to(ctx: &Context, state: &Path, sid: &str) -> LeaseEpoch {
        let attestation = sign_grant(ctx, state, sid, None);
        run_lease_grant(
            ctx,
            &LeaseGrantArgs {
                mission: mission(),
                request: None,
                to: Some(sid.to_owned()),
                ttl: None,
                granted_by: Some("test-operator".to_owned()),
                attestation: Some(attestation),
            },
        )
        .unwrap();
        leases(ctx)
            .unwrap()
            .current(&mission())
            .unwrap()
            .unwrap()
            .epoch
    }

    fn ping_primary(ctx: &Context, sid: &str, epoch: Option<u64>) -> anyhow::Result<()> {
        run_ping(
            ctx,
            &PingArgs {
                session: Some(sid.to_owned()),
                galaxy: "cosmon".to_owned(),
                role: Some("primary".to_owned()),
                mission: Some(mission()),
                epoch,
                ..PingArgs::default()
            },
        )
    }

    fn seat_of(dir: &Path, sid: &str) -> PilotRole {
        PresenceStore::new(dir)
            .scan()
            .unwrap()
            .into_iter()
            .find(|p| p.session_id.as_str() == sid)
            .expect("session has a snapshot")
            .role
    }

    // FAIL-CLOSED-AUTHORITY at the surface: before any grant, nobody may take
    // the primary seat, however confidently they ask.
    #[test]
    fn the_primary_seat_is_refused_before_any_grant() {
        let (_dir, ctx, state) = lease_world();
        let err = ping_primary(&ctx, "claude-sid", Some(1)).unwrap_err();
        assert!(err.to_string().contains("no lease"), "{err}");
        // And nothing was written — refused before it takes effect.
        assert!(PresenceStore::new(&state).scan().unwrap().is_empty());
    }

    #[test]
    fn a_granted_holder_may_take_the_seat_and_a_peer_may_not() {
        let (_dir, ctx, state) = lease_world();
        let epoch = grant_to(&ctx, &state, "claude-sid");

        ping_primary(&ctx, "claude-sid", Some(epoch.get())).unwrap();
        assert_eq!(seat_of(&state, "claude-sid"), PilotRole::Primary);

        // PRIMARY-UNIQUE: the concurrent attempt is refused, and leaves no
        // second primary behind.
        let err = ping_primary(&ctx, "codex-sid", Some(epoch.get())).unwrap_err();
        assert!(err.to_string().contains("held by claude-sid"), "{err}");
        let primaries = PresenceStore::new(&state)
            .scan()
            .unwrap()
            .into_iter()
            .filter(|p| p.role.is_primary())
            .count();
        assert_eq!(primaries, 1, "exactly one primary, always");
    }

    // The M4 acceptance clause "ancien primaire refusé après transfert",
    // observed through the surface a pilot actually uses.
    #[test]
    fn the_former_primary_is_demoted_by_its_own_next_heartbeat() {
        let (_dir, ctx, state) = lease_world();
        let first = grant_to(&ctx, &state, "claude-sid");
        ping_primary(&ctx, "claude-sid", Some(first.get())).unwrap();
        assert_eq!(seat_of(&state, "claude-sid"), PilotRole::Primary);

        // The operator transfers the controls.
        let second = grant_to(&ctx, &state, "codex-sid");
        assert_eq!(second, first.next());

        // A bare heartbeat from the old primary — no flags, the hook's ping.
        // It must not fail (the session is alive) and it must not keep the
        // seat (the session is not PRIMARY any more).
        run_ping(
            &ctx,
            &PingArgs {
                session: Some("claude-sid".to_owned()),
                galaxy: "cosmon".to_owned(),
                ..PingArgs::default()
            },
        )
        .unwrap();
        assert_eq!(seat_of(&state, "claude-sid"), PilotRole::Copilot);

        // An explicit re-claim at the epoch it used to hold is a refusal.
        let err = ping_primary(&ctx, "claude-sid", Some(first.get())).unwrap_err();
        assert!(err.to_string().contains("held by codex-sid"), "{err}");
    }

    #[test]
    fn a_primary_claim_must_name_a_mission_and_an_epoch() {
        let (_dir, ctx, state) = lease_world();
        grant_to(&ctx, &state, "claude-sid");

        let err = run_ping(
            &ctx,
            &PingArgs {
                session: Some("claude-sid".to_owned()),
                galaxy: "cosmon".to_owned(),
                role: Some("primary".to_owned()),
                ..PingArgs::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("needs --mission"), "{err}");

        let err = ping_primary(&ctx, "claude-sid", None).unwrap_err();
        assert!(err.to_string().contains("no epoch presented"), "{err}");
    }

    // "crash entre request et grant sans changement d'autorité" — through the
    // verbs, not the store.
    #[test]
    fn a_request_moves_no_authority() {
        let (_dir, ctx, state) = lease_world();
        let epoch = grant_to(&ctx, &state, "claude-sid");

        run_lease_request(
            &ctx,
            &LeaseRequestArgs {
                mission: mission(),
                to: Some("codex-sid".to_owned()),
                from: Some("codex-sid".to_owned()),
                reason: "claude is near its window limit".to_owned(),
            },
        )
        .unwrap();

        // …and the process dies here. The holder has not moved.
        let cur = leases(&ctx).unwrap().current(&mission()).unwrap().unwrap();
        assert_eq!(cur.holder_session_id.as_str(), "claude-sid");
        assert_eq!(cur.epoch, epoch);
        // The candidate still cannot take the seat.
        assert!(ping_primary(&ctx, "codex-sid", Some(epoch.get())).is_err());
    }

    #[test]
    fn granting_a_request_seats_the_session_it_named() {
        let (_dir, ctx, state) = lease_world();
        grant_to(&ctx, &state, "claude-sid");
        run_lease_request(
            &ctx,
            &LeaseRequestArgs {
                mission: mission(),
                to: Some("codex-sid".to_owned()),
                from: Some("codex-sid".to_owned()),
                reason: String::new(),
            },
        )
        .unwrap();

        let store = leases(&ctx).unwrap();
        let pending = store.unanswered_requests(&mission()).unwrap();
        assert_eq!(pending.len(), 1);

        let attestation = sign_grant(&ctx, &state, "codex-sid", None);
        run_lease_grant(
            &ctx,
            &LeaseGrantArgs {
                mission: mission(),
                request: Some(pending[0].id.as_str().to_owned()),
                to: None,
                ttl: None,
                granted_by: Some("test-operator".to_owned()),
                attestation: Some(attestation),
            },
        )
        .unwrap();

        let cur = store.current(&mission()).unwrap().unwrap();
        assert_eq!(cur.holder_session_id.as_str(), "codex-sid");
        assert_eq!(cur.request_id.as_ref(), Some(&pending[0].id));
        assert!(store.unanswered_requests(&mission()).unwrap().is_empty());
        ping_primary(&ctx, "codex-sid", Some(cur.epoch.get())).unwrap();
        assert_eq!(seat_of(&state, "codex-sid"), PilotRole::Primary);
    }

    #[test]
    fn granting_an_unknown_request_is_refused_and_writes_nothing() {
        let (_dir, ctx, _state) = lease_world();
        let err = run_lease_grant(
            &ctx,
            &LeaseGrantArgs {
                mission: mission(),
                request: Some("req-000000000000".to_owned()),
                to: None,
                ttl: None,
                granted_by: None,
                attestation: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no request"), "{err}");
        assert!(leases(&ctx).unwrap().current(&mission()).unwrap().is_none());
    }

    #[test]
    fn a_grant_with_neither_a_request_nor_a_session_says_so() {
        let (_dir, ctx, _state) = lease_world();
        let err = run_lease_grant(
            &ctx,
            &LeaseGrantArgs {
                mission: mission(),
                request: None,
                to: None,
                ttl: None,
                granted_by: None,
                attestation: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("nothing to grant"), "{err}");
    }

    // The acceptance clause on inspectable state and recovery: the ledger on
    // disk is enough to reconstruct who is PRIMARY, with `cat` and `jq`.
    #[test]
    fn the_authority_history_is_readable_on_disk() {
        let (_dir, ctx, state) = lease_world();
        grant_to(&ctx, &state, "claude-sid");
        grant_to(&ctx, &state, "codex-sid");

        let path = leases(&ctx).unwrap().grants_path(&mission());
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one line per grant, oldest first");
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["mission_id"], "task-20260731-9cf4");
            assert!(v["epoch"].is_number(), "epoch reads as a number in jq");
        }
        assert_eq!(lines[0].contains("claude-sid"), true);
        assert_eq!(lines[1].contains("codex-sid"), true);

        // A reader with no memory recomputes the same head.
        let fresh = PilotLeaseStore::new(&state).trusting(std::sync::Arc::new(
            MinisignOperatorVerifier::from_public_key_text(&operator().public_key_file(), "test")
                .unwrap(),
        ));
        assert_eq!(
            fresh
                .current(&mission())
                .unwrap()
                .unwrap()
                .holder_session_id
                .as_str(),
            "codex-sid",
        );
    }

    // A lease is per-mission: holding one does not seat you on another.
    #[test]
    fn authority_on_one_mission_is_not_authority_on_another() {
        let (_dir, ctx, state) = lease_world();
        let epoch = grant_to(&ctx, &state, "claude-sid");
        let other = MoleculeId::new("task-20260731-0c2d").unwrap();
        let err = run_ping(
            &ctx,
            &PingArgs {
                session: Some("claude-sid".to_owned()),
                galaxy: "cosmon".to_owned(),
                role: Some("primary".to_owned()),
                mission: Some(other),
                epoch: Some(epoch.get()),
                ..PingArgs::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no lease"), "{err}");
    }

    #[test]
    fn an_expired_lease_stops_seating_its_holder() {
        let (_dir, ctx, state) = lease_world();
        // A zero ttl is over on arrival: `is_valid_at` is `now < deadline`.
        // (A negative one is refused outright — the canonical challenge has no
        // encoding for a lease that expired before it was granted.)
        let attestation = sign_grant(&ctx, &state, "claude-sid", Some(0));
        run_lease_grant(
            &ctx,
            &LeaseGrantArgs {
                mission: mission(),
                request: None,
                to: Some("claude-sid".to_owned()),
                ttl: Some(0),
                granted_by: Some("test-operator".to_owned()),
                attestation: Some(attestation),
            },
        )
        .unwrap();
        let err = ping_primary(&ctx, "claude-sid", Some(1)).unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[test]
    fn show_runs_on_a_mission_with_and_without_a_lease() {
        let (_dir, ctx, state) = lease_world();
        let args = LeaseShowArgs {
            mission: mission(),
            history: true,
        };
        run_lease_show(&ctx, &args).unwrap();
        grant_to(&ctx, &state, "claude-sid");
        run_lease_show(&ctx, &args).unwrap();
    }

    // The mailbox and the lease are different files with different jobs: a
    // message never moves authority, and a grant never delivers a message.
    #[test]
    fn the_lease_and_the_mailbox_do_not_touch() {
        let (_dir, ctx, state) = lease_world();
        grant_to(&ctx, &state, "claude-sid");
        run_send(
            &ctx,
            &SendArgs {
                to: "claude-sid".to_owned(),
                from: Some("codex-sid".to_owned()),
                message: "give me the controls".to_owned(),
                expires_in: None,
            },
        )
        .unwrap();

        // The message is in the inbox; the ledger has exactly one grant and
        // the holder is unchanged.
        let mb = PilotMailbox::new(&state);
        assert_eq!(
            mb.pending(&SessionId::new("claude-sid").unwrap(), Utc::now())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(leases(&ctx).unwrap().grants(&mission()).unwrap().len(), 1);
        assert_eq!(
            leases(&ctx)
                .unwrap()
                .current(&mission())
                .unwrap()
                .unwrap()
                .holder_session_id
                .as_str(),
            "claude-sid",
        );
    }
}
