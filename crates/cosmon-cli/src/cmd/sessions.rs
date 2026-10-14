// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sessions` — the co-pilotage cockpit (mission co-pilotage M5).
//!
//! M1 gave the mission a provider-neutral way to *see* a session
//! ([`cosmon_session_probe`]), M2 a way to be *present* to one and message it,
//! M3 a way to hand one over ([`cosmon_pilot_checkpoint`]), M4 a way to say who
//! may fly it (`cosmon_core::pilot_lease`). Each of those is a library or a
//! hidden verb. This module is the one surface an operator types.
//!
//! # The one question this module answers
//!
//! *Which session am I talking about?*
//!
//! Everything else here is composition. The hard part is naming, and the
//! answer is one rule applied without exception: the canonical selector is
//! `<provider>:<native-session-id>`, and nothing else ever breaks a tie. A
//! display title, a tmux pane name, a cwd and a modification time are shown
//! because a human needs them to *recognise* a session; none of them is ever
//! read to *choose* one. When a name matches zero or two sessions, this module
//! prints the candidates and refuses — it never picks the most recent, which
//! is precisely the mission's falsifier 3 (*"two unnamed sessions in the same
//! cwd are confused"*).
//!
//! # Shape
//!
//! ```text
//!   discover ─┐                        ProbeRegistry  (M1)
//!   show ─────┤ provider logs, read-only
//!             │
//!   list ─────┐                        PresenceStore  (M2)
//!   peers ────┤ who is live, in which seat
//!   attach ───┘
//!             │
//!   send ─────┐                        PilotMailbox   (M2)
//!   inbox ────┘ traced envelopes, at-least-once
//!             │
//!   checkpoint┐                        CheckpointStore(M3)
//!   drift ────┘ hand-over records, tri-valued comparison
//!             │
//!   takeover ─┘                        PilotLeaseStore(M4)
//! ```
//!
//! # What this module deliberately does not do
//!
//! - **It does not replace `cs journal` or `cs pilot`.** The singular verb is
//!   the operator notebook and `cs pilot` is the cognitive REPL of ADR-115;
//!   both keep their bytes. This is a third thing (ADR-168 §D3.5).
//! - **It does not grant authority.** `takeover request` writes an ask;
//!   `takeover grant` is the operator's gesture. No quota reading, heartbeat
//!   gap or inference moves a lease (ADR-168 §D3.1).
//! - **It does not write to a provider's session.** Following a session opens
//!   its log for reading and nothing else — no keystroke, no pane, no rename
//!   (OBSERVATION-NEUTRE).
//! - **It does not branch on the provider.** Adding Gemini is a
//!   [`cosmon_session_probe::SessionProbe`] implementation registered in
//!   [`registry`]; no verb below
//!   knows how many providers exist (mission falsifier 10).

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use cosmon_core::id::{MoleculeId, SessionId};
use cosmon_core::operator_attestation::{GrantChallenge, OperatorGestureVerifier};
use cosmon_core::pilot_lease::{LeaseDecision, LeaseEpoch, RefusalReason, RequestId};
use cosmon_core::presence::Presence;
use cosmon_filestore::{FileStore, PresenceStore};
use cosmon_filestore::{MinisignOperatorVerifier, TAKEOVER_PUBKEY_ENV, TAKEOVER_PUBKEY_REL};
use cosmon_pilot_checkpoint::{
    compare, CheckpointStore, Claim, EvidenceRef, MissionId, PilotCheckpoint, Scope,
    SessionId as CheckpointSessionId, Stance,
};
use cosmon_session_probe::{
    ClaudeProbe, CodexProbe, Cursor, DiscoveryFilter, ProbeRegistry, ProviderSessionRef,
    RepoIdentity, RepoKind, SessionEventKind, SessionSelector,
};
use cosmon_state::{MoleculeFilter, StateStore};
use serde::{Deserialize, Serialize};

use super::presence;
use super::Context;

/// What `cs sessions --help` opens with, before the verbs and the examples.
///
/// It exists as a constant rather than a doc comment because the doc comment
/// is also the one line printed in `cs help`, and that line has to stay one
/// line. A reader who has never seen this surface meets the mechanism here:
/// two seats, one set of controls, and a human holding the key to them.
pub const LONG_ABOUT: &str = "\
Two agent sessions — a Claude and a Codex, or two of either — work the same
mission on this machine. One holds the controls and may change the mission;
the other reads the same material, compares, and advises, and can change
nothing. Both see each other, can write to each other, and leave a hand-over
note when they stop, so the next session resumes without re-reading the
whole conversation.

Passing the controls is never automatic. A session may ASK for them; only
you, the human, hand them over, by signing the request with your key. No
quota, timeout or heuristic moves them.

The verbs come in the order you meet them: find a session (discover, show),
take a seat (attach, list, peers), talk (send, inbox), hand over (checkpoint,
drift, takeover). `hook` wires the routine ones into the agent itself.
`lifecycle` accepts content-free observations from a client that already owns
a Codex app-server connection; cosmon does not open that connection. It
requires a presence row previously established by attach or a provider hook.

Both seats are live sessions of the SAME mission, and only a human signature
moves the controls between them. For separate molecules — each with its own
lifecycle, visible in `cs peek` and harvested on its own — exchanging bounded
findings instead, see `cs work` (ADR-182).";

/// Top-level arguments for `cs sessions`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// The cockpit verbs.
///
/// The order is the order an operator meets them: find a session, look at it,
/// take a seat beside it, talk to it, hand over from it.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Launch Claude or Codex with a fresh identity shared by hooks and tools.
    Launch(super::sessions_launch::Args),
    /// Record a content-free lifecycle event from a connected provider client.
    Lifecycle(super::sessions_lifecycle::Args),
    /// Which agent conversations exist on this machine, and the exact name
    /// to refer to one by.
    Discover(DiscoverArgs),
    /// Which of them have taken a seat, and in which role.
    List(ListArgs),
    /// Ledger-derived pilot authority in one versioned JSON document.
    Seats(SeatsArgs),
    /// Look inside one conversation — its last events, read-only.
    Show(ShowArgs),
    /// Take a seat: say "I am here, in this role". Until you do, the others
    /// cannot see you.
    Attach(AttachArgs),
    /// Who is seated around this session, and which way each one faces.
    Peers(PeersArgs),
    /// Write one message to another session. Delivered, and consumed, once.
    Send(SendArgs),
    /// Read the messages addressed to this session (`--peek` to look without
    /// consuming them).
    Inbox(InboxArgs),
    /// Leave — or read — the note that lets someone else resume this mission.
    #[command(subcommand)]
    Checkpoint(CheckpointSub),
    /// Compare what two sessions concluded — `AGREE`, `FINDING` or
    /// `INCONCLUSIVE`, never a score.
    Drift(DriftArgs),
    /// The controls: who may change the mission, who asked for them, and the
    /// signature that hands them over.
    #[command(subcommand)]
    Takeover(TakeoverSub),
    /// Wire the routine gestures — take a seat, read the mailbox, leave a
    /// note — into the agent itself, so they happen without being typed.
    Hook(super::sessions_hook::Args),
}

/// Arguments for `cs sessions discover`.
#[derive(clap::Args, Default)]
pub struct DiscoverArgs {
    /// Repository whose sessions to show. Defaults to the repository the
    /// current directory is in. Resolved to an exact checkout — a worktree is
    /// never its canonical checkout (REPO-EXACT).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
    /// Show sessions whose recorded working directory is exactly this path.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
    /// Show every session every adapter can see, from any repository.
    #[arg(long)]
    pub all: bool,
    /// Restrict to one provider (`claude`, `codex`, …).
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
}

/// Arguments for `cs sessions list`.
#[derive(clap::Args, Default)]
pub struct ListArgs {
    /// Filter to one galaxy.
    #[arg(long)]
    pub galaxy: Option<String>,
    /// Show only pilots in this seat (`primary` or `copilot`).
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    /// Show only pilots co-piloting this session.
    #[arg(long, value_name = "SID")]
    pub follows: Option<String>,
    /// Include snapshots whose heartbeat has gone stale.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `cs sessions seats`.
#[derive(clap::Args, Default)]
pub struct SeatsArgs {
    /// Scan every project under `$COSMON_CLUSTER_ROOT` using the `cs tail`
    /// cross-galaxy perimeter.
    #[arg(long)]
    pub all_galaxies: bool,
}

/// The versioned read contract emitted by `cs sessions seats --json`.
#[derive(Debug, Serialize)]
struct SeatsDocument {
    schema_version: u32,
    galaxies: Vec<GalaxySeats>,
}

/// Seat findings for one galaxy.  Readable adverse findings remain data.
#[derive(Debug, Serialize)]
struct GalaxySeats {
    galaxy: String,
    state: &'static str,
    missions: Vec<MissionSeats>,
}

/// Authority and claims for one pilot mission.
#[derive(Debug, Serialize)]
struct MissionSeats {
    mission: String,
    verdict: &'static str,
    seats: Vec<SeatRow>,
}

/// One presence claim, deliberately retained even when it is stale or wrong.
#[derive(Debug, Serialize)]
struct SeatRow {
    session: String,
    provider: Option<String>,
    native_id: Option<String>,
    claimed_role: &'static str,
    epoch: Option<u64>,
    liveness: bool,
    heartbeat: DateTime<Utc>,
    standing: &'static str,
}

/// Arguments for `cs sessions show`.
#[derive(clap::Args)]
pub struct ShowArgs {
    /// The canonical selector, `<provider>:<native-session-id>`.
    #[arg(value_name = "SELECTOR")]
    pub selector: String,
    /// Print the last N normalised events (kinds and sizes — never content).
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub tail: usize,
    /// Skip reading the log; show only what discovery already knows.
    #[arg(long)]
    pub no_read: bool,
}

/// Arguments for `cs sessions attach`.
#[derive(clap::Args, Default)]
pub struct AttachArgs {
    /// Seat to take: `copilot` (default) or `primary`. A primary seat is
    /// checked against the mission's lease ledger and refused if it is not
    /// this session's to take.
    #[arg(long, value_name = "ROLE", default_value = "copilot")]
    pub role: String,
    /// The pilot this session is co-piloting — a cosmon session id, or a
    /// `<provider>:<native-session-id>` selector that a live pilot advertises.
    #[arg(long, value_name = "SID_OR_SELECTOR")]
    pub follow: Option<String>,
    /// This session's cosmon id. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// The provider session this pilot is driving, as a canonical selector.
    /// Equivalent to `--provider` + `--native-session-id`.
    #[arg(long = "as", value_name = "SELECTOR")]
    pub as_selector: Option<String>,
    /// Provider half of this session's key, when not using `--as`.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Native id half of this session's key, when not using `--as`.
    #[arg(long = "native-session-id", value_name = "ID")]
    pub native_session_id: Option<String>,
    /// Mission this seat is about. Required for a primary seat.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: Option<MoleculeId>,
    /// The lease epoch this pilot believes it holds. Required for a primary
    /// seat: a claim that names no generation is not a claim.
    #[arg(long, value_name = "N")]
    pub epoch: Option<u64>,
    /// A capability this pilot advertises. Repeatable.
    #[arg(long = "capability", value_name = "TOKEN")]
    pub capabilities: Vec<String>,
    /// One line describing what this pilot is doing.
    #[arg(long)]
    pub headline: Option<String>,
    /// Galaxy label to record.
    #[arg(long, default_value = "cosmon")]
    pub galaxy: String,
}

/// Arguments for `cs sessions peers`.
#[derive(clap::Args, Default)]
pub struct PeersArgs {
    /// The session whose neighbourhood to show. Defaults to
    /// `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// Include snapshots whose heartbeat has gone stale.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `cs sessions send`.
#[derive(clap::Args, Default)]
pub struct SendArgs {
    /// Destination — a cosmon session id, or a selector a live pilot
    /// advertises.
    #[arg(long, value_name = "SID_OR_SELECTOR")]
    pub to: String,
    /// The message. Stored content-addressed; the envelope carries its hash.
    #[arg(long, value_name = "TEXT")]
    pub message: String,
    /// Sender session id. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub from: Option<String>,
    /// Seconds after which an unread envelope reads as `expired` rather than
    /// as a fresh instruction.
    #[arg(long = "expires-in", value_name = "SECONDS")]
    pub expires_in: Option<i64>,
}

/// Arguments for `cs sessions inbox`.
#[derive(clap::Args, Default)]
pub struct InboxArgs {
    /// Mailbox to read. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// Show pending envelopes without acknowledging them.
    #[arg(long)]
    pub peek: bool,
    /// Include already-acknowledged envelopes.
    #[arg(long)]
    pub all: bool,
    /// Keep reading, printing each envelope as it arrives, until interrupted.
    #[arg(long)]
    pub follow: bool,
    /// Seconds between polls under `--follow`.
    #[arg(long, value_name = "SECONDS", default_value_t = 2)]
    pub interval: u64,
}

/// `cs sessions checkpoint <sub>` — the hand-over record of ADR-168 §D5.
///
/// `Publish` is much the widest variant and clippy would rather it were boxed.
/// It cannot be: clap's `Subcommand` derive requires the variant's field to
/// implement `Args`, which `Box<T>` does not. The enum is built once per
/// process, so the width costs one stack frame per `cs` invocation.
#[allow(clippy::large_enum_variant)]
#[derive(clap::Subcommand)]
pub enum CheckpointSub {
    /// Publish this pilot's hand-over record for a mission.
    Publish(CheckpointPublishArgs),
    /// Write the same record as a draft, for the hook to publish at the next
    /// natural transition. Takes exactly the flags `publish` takes.
    Stage(CheckpointPublishArgs),
    /// List the checkpoints published for a mission.
    List(CheckpointListArgs),
    /// Show one checkpoint in full.
    Show(CheckpointShowArgs),
}

/// Arguments for `cs sessions checkpoint publish`.
#[derive(clap::Args, Default)]
pub struct CheckpointPublishArgs {
    /// The mission being flown.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: String,
    /// Publishing session. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// The authority epoch the publisher believes it is under. Defaults to
    /// the epoch on its own presence snapshot, then to 0.
    #[arg(long, value_name = "N")]
    pub epoch: Option<u64>,
    /// Identifier for this checkpoint. Defaults to a timestamped id.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
    /// Something this mission covers. Repeatable.
    #[arg(long = "include", value_name = "TEXT")]
    pub includes: Vec<String>,
    /// Something this mission explicitly does not cover. Repeatable.
    #[arg(long = "exclude", value_name = "TEXT")]
    pub excludes: Vec<String>,
    /// A position currently held, as `SUBJECT[:affirm|deny]=STATEMENT`.
    /// Repeatable.
    #[arg(long = "hypothesis", value_name = "CLAIM")]
    pub hypotheses: Vec<String>,
    /// An intended next move, in the same `SUBJECT[:STANCE]=STATEMENT` form.
    /// Repeatable — this is the list a co-pilot's contradiction is found in.
    #[arg(long = "next", value_name = "CLAIM")]
    pub next_actions: Vec<String>,
    /// Something already done, in the pilot's words. Repeatable.
    #[arg(long = "done", value_name = "TEXT")]
    pub completed: Vec<String>,
    /// A known risk. Repeatable.
    #[arg(long = "risk", value_name = "TEXT")]
    pub risks: Vec<String>,
    /// A question the pilot could not answer. Repeatable — this is where
    /// uncertainty belongs, never inside a stance.
    #[arg(long = "question", value_name = "TEXT")]
    pub questions: Vec<String>,
    /// Evidence for one claim, as `SUBJECT=LOCATOR[#DIGEST]`. Repeatable.
    #[arg(long = "evidence", value_name = "SUBJECT=LOCATOR")]
    pub evidence: Vec<String>,
    /// Evidence for the checkpoint as a whole, as `LOCATOR[#DIGEST]`.
    /// Repeatable.
    #[arg(long = "checkpoint-evidence", value_name = "LOCATOR")]
    pub checkpoint_evidence: Vec<String>,
    /// Read versioned checkpoint content from a JSON file, or `-` for stdin.
    /// Cosmon, rather than the file, stamps all publication provenance.
    #[arg(
        long,
        value_name = "PATH|-",
        conflicts_with_all = [
            "epoch", "id", "includes", "excludes", "hypotheses", "next_actions", "completed",
            "risks", "questions", "evidence", "checkpoint_evidence"
        ]
    )]
    pub from_file: Option<PathBuf>,
}

/// Arguments for `cs sessions checkpoint list`.
#[derive(clap::Args, Default)]
pub struct CheckpointListArgs {
    /// The mission whose checkpoints to list.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: String,
    /// Show only what this session published.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
}

/// Arguments for `cs sessions checkpoint show`.
#[derive(clap::Args, Default)]
pub struct CheckpointShowArgs {
    /// The mission the checkpoint belongs to.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: String,
    /// The checkpoint id. Omit to take the latest published by `--session`.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
    /// The publishing session, when selecting by recency rather than by id.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// Select only the newest checkpoint matching the current verified lease
    /// holder and epoch. Requires `--json` so absence is a typed null.
    #[arg(long, conflicts_with_all = ["id", "session"])]
    pub of_holder: bool,
}

/// Arguments for `cs sessions drift`.
#[derive(clap::Args)]
pub struct DriftArgs {
    /// The first session.
    #[arg(value_name = "SESSION_A")]
    pub session_a: String,
    /// The second session.
    #[arg(value_name = "SESSION_B")]
    pub session_b: String,
    /// The mission both sides checkpointed.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: String,
    /// Which checkpoint of each session to compare. Only `latest` is a
    /// selector; name an exact record with `--checkpoint-a` / `--checkpoint-b`.
    #[arg(long, value_name = "latest", default_value = "latest")]
    pub checkpoint: String,
    /// Exact checkpoint id for side A.
    #[arg(long = "checkpoint-a", value_name = "ID")]
    pub checkpoint_a: Option<String>,
    /// Exact checkpoint id for side B.
    #[arg(long = "checkpoint-b", value_name = "ID")]
    pub checkpoint_b: Option<String>,
}

/// `cs sessions takeover <sub>` — the supervised transfer of ADR-168 §D6.
///
/// The same four verbs `cs presence lease` has, and deliberately no fifth:
/// a transfer is an ask followed by an operator's decision. There is nothing
/// here for a quota heuristic to call.
#[derive(clap::Subcommand)]
pub enum TakeoverSub {
    /// Who holds the controls, at which epoch, and what has been asked.
    Show(TakeoverShowArgs),
    /// Ask for the controls. Writes a request and confers nothing.
    Request(TakeoverRequestArgs),
    /// Hand the controls over — your signature, which no agent can produce.
    Grant(TakeoverGrantArgs),
    /// Print the exact bytes an operator signs to authorise one transfer.
    Challenge(TakeoverChallengeArgs),
    /// Show which operator key this galaxy trusts to authorise a transfer.
    Trust(TakeoverTrustArgs),
    /// Ask whether a session may pilot: the ledger's verdict, plus whether its
    /// seat would actually present that epoch. Exits 0 or 1.
    Check(TakeoverCheckArgs),
}

/// Arguments for `cs sessions takeover show`.
#[derive(clap::Args)]
pub struct TakeoverShowArgs {
    /// Mission whose lease to inspect.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Print every grant ever recorded instead of only the head.
    #[arg(long)]
    pub history: bool,
}

/// Arguments for `cs sessions takeover request`.
#[derive(clap::Args)]
pub struct TakeoverRequestArgs {
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

/// Arguments for `cs sessions takeover grant`.
#[derive(clap::Args)]
pub struct TakeoverGrantArgs {
    /// Mission whose controls are being handed over.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Request being answered. The holder is taken from the request.
    #[arg(long, value_name = "REQUEST_ID")]
    pub request: Option<String>,
    /// Session to seat, when granting without a request.
    #[arg(long, value_name = "SID")]
    pub to: Option<String>,
    /// Seconds after which the lease authorises nothing.
    #[arg(long = "ttl", value_name = "SECONDS")]
    pub ttl: Option<i64>,
    /// Operator identity to record. Defaults to `$USER`. Covered by the
    /// attestation, so it is a signed claim and not a free string.
    #[arg(long = "by", value_name = "NAME")]
    pub granted_by: Option<String>,
    /// The operator's detached minisign signature over the challenge, or `-`
    /// for stdin. Required: `--by` is a label, the signature is the gesture.
    #[arg(long, value_name = "PATH")]
    pub attestation: Option<PathBuf>,
    /// The operator's minisign **secret** key. Folds challenge, signature and
    /// grant into this one command: the transfer is printed for you to read,
    /// `minisign(1)` asks for your passphrase, and no `.minisig` is left
    /// behind. cosmon still owns no signer — it relays to yours.
    #[arg(
        long = "sign-with",
        value_name = "PATH",
        conflicts_with = "attestation"
    )]
    pub sign_with: Option<PathBuf>,
}

/// Arguments for `cs sessions takeover challenge`.
#[derive(clap::Args)]
pub struct TakeoverChallengeArgs {
    /// Mission whose controls would be handed over.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Request being answered. The holder is taken from the request.
    #[arg(long, value_name = "REQUEST_ID")]
    pub request: Option<String>,
    /// Session that would be seated, when there is no request to answer.
    #[arg(long, value_name = "SID")]
    pub to: Option<String>,
    /// Seconds after which the lease would authorise nothing.
    #[arg(long = "ttl", value_name = "SECONDS")]
    pub ttl: Option<i64>,
    /// Operator identity the grant would claim. Defaults to `$USER`.
    #[arg(long = "by", value_name = "NAME")]
    pub granted_by: Option<String>,
}

/// Arguments for `cs sessions takeover trust`.
#[derive(clap::Args)]
pub struct TakeoverTrustArgs {}

/// Arguments for `cs sessions takeover check`.
#[derive(clap::Args)]
pub struct TakeoverCheckArgs {
    /// Mission the gesture would touch.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub mission: MoleculeId,
    /// Session issuing the gesture. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// The epoch the caller believes it holds. Omitting it is itself a
    /// refusal.
    #[arg(long, value_name = "N")]
    pub epoch: Option<u64>,
}

/// Dispatch a `cs sessions <sub>` invocation.
///
/// # Errors
///
/// Propagates every failure of the underlying registries, plus the two this
/// module owns: a selector that names no session, and a selector that names
/// more than one.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Discover(a) => run_discover(ctx, a),
        Sub::List(a) => run_list(ctx, a),
        Sub::Seats(a) => run_seats(ctx, a),
        Sub::Show(a) => run_show(ctx, a),
        Sub::Attach(a) => run_attach(ctx, a),
        Sub::Peers(a) => run_peers(ctx, a),
        Sub::Send(a) => run_send(ctx, a),
        Sub::Inbox(a) => run_inbox(ctx, a),
        Sub::Checkpoint(a) => run_checkpoint(ctx, a),
        Sub::Drift(a) => run_drift(ctx, a),
        Sub::Takeover(a) => run_takeover(ctx, a),
        Sub::Launch(a) => super::sessions_launch::run(a),
        Sub::Lifecycle(a) => super::sessions_lifecycle::run(ctx, a),
        Sub::Hook(a) => super::sessions_hook::run(ctx, a),
    }
}

// ---------------------------------------------------------------------------
// The registry — the whole provider-specific surface of this module
// ---------------------------------------------------------------------------

/// Build the probe registry this cockpit talks to.
///
/// Two adapters today, and the *only* place in `cs sessions` that names a
/// provider. Both roots may be overridden by an environment variable, because
/// a worker's Claude configuration directory is not always the ambient one
/// (`~/.claude-accounts/<email>/`) and because a test needs a tree it owns.
///
/// A probe whose root cannot be resolved is dropped rather than fatal: a host
/// without Codex installed has zero Codex sessions, not a broken cockpit. That
/// is also why this is infallible — "I can see nothing" is an answer, and an
/// empty registry renders as an empty listing rather than as an error.
#[must_use]
pub fn registry() -> ProbeRegistry {
    let mut reg = ProbeRegistry::new();

    let claude = match std::env::var("COSMON_SESSIONS_CLAUDE_ROOT") {
        Ok(root) if !root.trim().is_empty() => ClaudeProbe::new(root).ok(),
        _ => ClaudeProbe::from_home().ok(),
    };
    if let Some(p) = claude {
        reg = reg.with(Box::new(p));
    }

    let codex = match std::env::var("COSMON_SESSIONS_CODEX_ROOT") {
        Ok(root) if !root.trim().is_empty() => CodexProbe::new(root).ok(),
        _ => CodexProbe::from_home().ok(),
    };
    if let Some(p) = codex {
        reg = reg.with(Box::new(p));
    }

    reg
}

// ---------------------------------------------------------------------------
// Naming — the part that must never guess
// ---------------------------------------------------------------------------

/// Resolve a canonical selector to exactly one provider session.
///
/// Three outcomes, and two of them are errors on purpose:
///
/// - **one match** — the answer;
/// - **no match** — an error that *lists the sessions that do exist* for that
///   provider, so the operator can copy one, rather than a bare "not found";
/// - **more than one match** — an error that lists both source logs. A cockpit
///   that picked the most recently modified one here would satisfy the command
///   and violate the invariant the command exists to protect.
fn resolve_session(
    reg: &ProbeRegistry,
    raw: &str,
    filter: &DiscoveryFilter,
) -> anyhow::Result<ProviderSessionRef> {
    let selector: SessionSelector = raw.parse().map_err(|e| {
        anyhow::anyhow!(
            "{e} — a session is named `<provider>:<native-session-id>`; \
             `cs sessions discover` prints the ones on this host"
        )
    })?;

    let mut found = reg.candidates(&selector)?;
    found.retain(|s| filter.accepts(s));

    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(anyhow::anyhow!(
            "no session {selector}{}",
            candidate_hint(reg, &selector)
        )),
        n => Err(anyhow::anyhow!(
            "{n} sessions answer to {selector} — refusing to choose:\n{}",
            found
                .iter()
                .map(|s| format!("  {}", s.source_locator.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

/// The "did you mean" tail of a no-match error: the selectors that do exist,
/// same provider first, capped so a host with hundreds of logs still prints a
/// readable error.
fn candidate_hint(reg: &ProbeRegistry, wanted: &SessionSelector) -> String {
    const MAX: usize = 8;
    let Ok(all) = reg.discover(&DiscoveryFilter::all()) else {
        return String::new();
    };
    let mut same: Vec<String> = all
        .iter()
        .filter(|s| s.provider == wanted.provider)
        .map(|s| s.selector().to_string())
        .collect();
    same.sort();
    if same.is_empty() {
        let providers: Vec<String> = reg.providers().iter().map(ToString::to_string).collect();
        return format!(
            " — no {} session is visible at all (registered providers: {})",
            wanted.provider,
            if providers.is_empty() {
                "none".to_owned()
            } else {
                providers.join(", ")
            },
        );
    }
    let shown = same.len().min(MAX);
    let more = same.len().saturating_sub(shown);
    format!(
        " — candidates:\n{}{}",
        same[..shown]
            .iter()
            .map(|s| format!("  {s}"))
            .collect::<Vec<_>>()
            .join("\n"),
        if more > 0 {
            format!("\n  … and {more} more (`cs sessions discover --all`)")
        } else {
            String::new()
        },
    )
}

/// Resolve a cosmon session id, accepting a canonical selector as an alias.
///
/// A selector is resolved through the **presence registry**, not through the
/// provider logs: what is being named is a pilot with a mailbox, and a session
/// nobody has attached to has neither. Zero or two matches are errors that
/// list the live pilots; nothing is auto-selected.
fn resolve_pilot_sid(ctx: &Context, raw: &str) -> anyhow::Result<SessionId> {
    if !raw.contains(':') {
        return Ok(SessionId::new(raw.to_owned())?);
    }

    let selector: SessionSelector = raw.parse().map_err(|e| {
        anyhow::anyhow!("{e} — pass a cosmon session id, or a `<provider>:<native-id>` selector")
    })?;
    let rows = presence::store(ctx).scan()?;
    let mut matching: Vec<&Presence> = rows
        .iter()
        .filter(|p| p.selector().as_deref() == Some(raw))
        .collect();

    match matching.len() {
        1 => Ok(matching.remove(0).session_id.clone()),
        0 => Err(anyhow::anyhow!(
            "no live pilot advertises {selector}{}",
            pilot_hint(&rows)
        )),
        n => Err(anyhow::anyhow!(
            "{n} pilots advertise {selector} — refusing to choose:\n{}",
            matching
                .iter()
                .map(|p| format!("  {}", p.session_id.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

/// The "did you mean" tail for a pilot that is not in the presence registry.
fn pilot_hint(rows: &[Presence]) -> String {
    if rows.is_empty() {
        return " — the presence registry is empty (`cs sessions attach` takes a seat)".to_owned();
    }
    format!(
        " — pilots present:\n{}",
        rows.iter()
            .map(|p| format!(
                "  {sid}{sel}",
                sid = p.session_id.as_str(),
                sel = p.selector().map_or_else(String::new, |s| format!(" [{s}]")),
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

// ---------------------------------------------------------------------------
// discover / show — the provider side
// ---------------------------------------------------------------------------

/// Build the discovery filter a `discover` invocation asked for.
///
/// `--all` means no repository filter. Otherwise the filter is the repository
/// of `--repo`, or of the current directory — resolved to an exact identity,
/// never a path comparison.
fn discovery_filter(args: &DiscoverArgs) -> anyhow::Result<DiscoveryFilter> {
    let mut filter = DiscoveryFilter::all();
    if !args.all {
        let start = args
            .repo
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let repo = RepoIdentity::resolve(&start).ok_or_else(|| {
            anyhow::anyhow!(
                "{} is not inside a git repository — pass --repo <PATH> or --all",
                start.display()
            )
        })?;
        filter.repo = Some(repo);
    }
    if let Some(cwd) = &args.cwd {
        filter.cwd = Some(cwd.clone());
    }
    Ok(filter)
}

fn run_discover(ctx: &Context, args: &DiscoverArgs) -> anyhow::Result<()> {
    let reg = registry();
    let filter = discovery_filter(args)?;
    let mut rows = reg.discover(&filter)?;
    if let Some(want) = &args.provider {
        rows.retain(|s| s.provider.as_str() == want);
    }
    // Newest first, and the selector breaks ties so the order is total and
    // does not depend on directory enumeration.
    rows.sort_by(|a, b| {
        b.last_observed_at
            .cmp(&a.last_observed_at)
            .then_with(|| a.selector().cmp(&b.selector()))
    });

    if ctx.json {
        let mut out = std::io::stdout().lock();
        for s in &rows {
            writeln!(out, "{}", session_json(s))?;
        }
        return Ok(());
    }

    if rows.is_empty() {
        println!("(no provider sessions match)");
        return Ok(());
    }
    println!("{:<48}  {:<10}  {:<8}  CWD", "SELECTOR", "LAST", "REPO");
    let now = Utc::now();
    for s in &rows {
        println!(
            "{:<48}  {:<10}  {:<8}  {}",
            s.selector().to_string(),
            s.last_observed_at
                .map_or_else(|| "-".to_owned(), |t| presence::format_age(now, t)),
            s.repo_identity.as_ref().map_or("-", |r| match r.kind() {
                RepoKind::Canonical => "checkout",
                RepoKind::Worktree => "worktree",
            }),
            s.cwd
                .as_ref()
                .map_or_else(|| "-".to_owned(), |c| c.display().to_string()),
        );
    }
    Ok(())
}

/// One provider session as JSON, selector first.
fn session_json(s: &ProviderSessionRef) -> serde_json::Value {
    serde_json::json!({
        "selector": s.selector().to_string(),
        "provider": s.provider.to_string(),
        "native_session_id": s.native_session_id.to_string(),
        "repo_root": s.repo_identity.as_ref().map(|r| r.root().display().to_string()),
        "repo_kind": s.repo_identity.as_ref().map(|r| match r.kind() {
            RepoKind::Canonical => "checkout",
            RepoKind::Worktree => "worktree",
        }),
        "repo_linked_root": s
            .repo_identity
            .as_ref()
            .and_then(RepoIdentity::linked_root)
            .map(|p| p.display().to_string()),
        "cwd": s.cwd.as_ref().map(|c| c.display().to_string()),
        "source_locator": s.source_locator.display().to_string(),
        "display_name": s.display_name,
        "started_at": s.started_at,
        "last_observed_at": s.last_observed_at,
    })
}

/// A stable, provider-neutral label for an event kind.
///
/// Every arm is a name this cockpit prints, and the catch-all is load-bearing:
/// [`SessionEventKind`] is `#[non_exhaustive]`, so a kind added by a later
/// adapter counts as `other` here instead of failing to compile a cockpit that
/// has no opinion about it.
fn kind_label(kind: &SessionEventKind) -> String {
    match kind {
        SessionEventKind::SessionStarted { .. } => "session_started".to_owned(),
        SessionEventKind::UserMessage { .. } => "user_message".to_owned(),
        SessionEventKind::AssistantMessage { .. } => "assistant_message".to_owned(),
        SessionEventKind::TokenUsage { .. } => "token_usage".to_owned(),
        SessionEventKind::Quota(_) => "quota".to_owned(),
        SessionEventKind::ContextCompacted => "context_compacted".to_owned(),
        SessionEventKind::Other { record } => format!("other:{record}"),
        SessionEventKind::Unparseable => "unparseable".to_owned(),
        _ => "other".to_owned(),
    }
}

/// What one read of a session's log says about it, in the only terms this
/// cockpit publishes: counts, times and counters — never a sentence anyone
/// wrote.
#[derive(Default)]
struct ReadSummary {
    counts: BTreeMap<String, usize>,
    last_at: Option<DateTime<Utc>>,
    quota: Option<cosmon_session_probe::QuotaReading>,
    tail: Vec<String>,
}

/// Read `session` from the start and fold it into a [`ReadSummary`].
///
/// The whole log, every time: this is a snapshot verb, not a follower, and a
/// cursor kept between invocations would be a claim about a file that the file
/// can invalidate (ADR-168 §D4).
fn read_summary(
    reg: &ProbeRegistry,
    session: &ProviderSessionRef,
    tail: usize,
) -> anyhow::Result<ReadSummary> {
    let probe = reg
        .probe_for(&session.provider)
        .ok_or_else(|| anyhow::anyhow!("no adapter for provider {}", session.provider))?;
    let read = probe.read(session, Cursor::start())?;

    let mut out = ReadSummary::default();
    for ev in &read.events {
        *out.counts.entry(kind_label(&ev.kind)).or_default() += 1;
        out.last_at = ev.at.or(out.last_at);
        if let SessionEventKind::Quota(q) = &ev.kind {
            out.quota = Some(*q);
        }
    }
    if tail > 0 {
        let start = read.events.len().saturating_sub(tail);
        out.tail = read.events[start..]
            .iter()
            .map(|ev| {
                format!(
                    "{at}  @{offset}  {kind}",
                    at = ev.at.map_or_else(|| "-".to_owned(), |t| t.to_rfc3339()),
                    offset = ev.offset,
                    kind = kind_label(&ev.kind),
                )
            })
            .collect();
    }
    Ok(out)
}

fn run_show(ctx: &Context, args: &ShowArgs) -> anyhow::Result<()> {
    let reg = registry();
    let session = resolve_session(&reg, &args.selector, &DiscoveryFilter::all())?;

    // The presence row, if some pilot has attached to this provider session.
    // Absent is an ordinary answer: a session nobody co-pilots is still a
    // session.
    let selector = session.selector().to_string();
    let attached: Option<Presence> = presence::store(ctx)
        .scan()?
        .into_iter()
        .find(|p| p.selector().as_deref() == Some(selector.as_str()));

    let ReadSummary {
        counts,
        last_at,
        quota,
        tail,
    } = if args.no_read {
        ReadSummary::default()
    } else {
        read_summary(&reg, &session, args.tail)?
    };

    if ctx.json {
        let mut v = session_json(&session);
        let obj = v.as_object_mut().expect("session_json is an object");
        obj.insert("event_counts".to_owned(), serde_json::to_value(&counts)?);
        obj.insert("last_event_at".to_owned(), serde_json::to_value(last_at)?);
        obj.insert("quota".to_owned(), serde_json::to_value(quota)?);
        obj.insert(
            "attached_pilot".to_owned(),
            serde_json::to_value(attached.as_ref())?,
        );
        if args.tail > 0 {
            obj.insert("tail".to_owned(), serde_json::to_value(&tail)?);
        }
        println!("{v}");
        return Ok(());
    }

    println!("{}", session.selector());
    println!("  log       {}", session.source_locator.display());
    if let Some(cwd) = &session.cwd {
        println!("  cwd       {}", cwd.display());
    }
    match &session.repo_identity {
        Some(r) => println!(
            "  repo      {} ({})",
            r.root().display(),
            match r.kind() {
                RepoKind::Canonical => "canonical checkout",
                RepoKind::Worktree => "worktree",
            }
        ),
        None => println!("  repo      (none resolved)"),
    }
    if let Some(name) = &session.display_name {
        println!("  title     {name} (alias only — never used to break a tie)");
    }
    if let Some(t) = session.started_at {
        println!("  started   {t}");
    }
    match &attached {
        Some(p) => println!(
            "  pilot     {sid} role={role}{mission}",
            sid = p.session_id.as_str(),
            role = p.role.as_str(),
            mission = p
                .mission
                .as_ref()
                .map_or_else(String::new, |m| format!(" mission={}", m.as_str())),
        ),
        None => println!("  pilot     (no cosmon pilot has attached to this session)"),
    }
    if args.no_read {
        return Ok(());
    }
    println!("  events    {}", render_counts(&counts));
    println!(
        "  last      {}",
        last_at.map_or_else(|| "-".to_owned(), |t| t.to_rfc3339())
    );
    // Absence of a quota reading is *unknown*, never *fine*: Claude publishes
    // no proactive quota signal at all (ADR-168, trace A).
    match quota {
        Some(q) => println!(
            "  quota     used={} window={} resets={} limit_reached={}",
            q.used_percent
                .map_or_else(|| "?".to_owned(), |p| format!("{p:.1}%")),
            q.window_minutes
                .map_or_else(|| "?".to_owned(), |m| format!("{m}m")),
            q.resets_at_epoch
                .map_or_else(|| "?".to_owned(), |e| e.to_string()),
            q.limit_reached,
        ),
        None => println!("  quota     unknown — this provider published no reading"),
    }
    for line in &tail {
        println!("  {line}");
    }
    Ok(())
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "(none)".to_owned();
    }
    counts
        .iter()
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// list / attach / peers — the presence side
// ---------------------------------------------------------------------------

fn run_list(ctx: &Context, args: &ListArgs) -> anyhow::Result<()> {
    let now = Utc::now();
    let mut rows = presence::store(ctx).scan()?;
    if !args.all {
        rows.retain(|p| p.is_live(now));
    }
    if let Some(galaxy) = &args.galaxy {
        rows.retain(|p| &p.galaxy == galaxy);
    }
    if let Some(raw) = &args.role {
        let want = presence::parse_role(raw)?;
        rows.retain(|p| p.role == want);
    }
    if let Some(sid) = &args.follows {
        rows.retain(|p| p.follows.as_ref().is_some_and(|f| f.as_str() == sid));
    }
    rows.sort_by_key(|p| std::cmp::Reverse(p.heartbeat_at));

    if ctx.json {
        let mut out = std::io::stdout().lock();
        for p in &rows {
            writeln!(out, "{}", serde_json::to_value(p)?)?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no pilots present)");
        return Ok(());
    }
    println!(
        "{:<24}  {:<40}  {:<8}  {:<20}  {:<20}  {:>6}  AGE",
        "SESSION", "SELECTOR", "ROLE", "FOLLOWS", "MISSION", "EPOCH",
    );
    for p in &rows {
        println!(
            "{:<24}  {:<40}  {:<8}  {:<20}  {:<20}  {:>6}  {}",
            p.session_id.as_str(),
            p.selector().unwrap_or_else(|| "-".to_owned()),
            p.role.as_str(),
            p.follows.as_ref().map_or("-", SessionId::as_str),
            p.mission.as_ref().map_or("-", MoleculeId::as_str),
            p.lease_epoch
                .map_or_else(|| "-".to_owned(), |e| e.to_string()),
            presence::format_age(now, p.heartbeat_at),
        );
    }
    Ok(())
}

/// Render the cross-galaxy seat projection.
///
/// Presence is intentionally projected as a claim.  The ledger chooses a
/// holder; a heartbeat only supplies liveness and makes conflicting claims
/// visible to an operator.
fn run_seats(ctx: &Context, args: &SeatsArgs) -> anyhow::Result<()> {
    if !ctx.json {
        return Err(anyhow::anyhow!(
            "cs sessions seats is a machine-readable contract; pass --json"
        ));
    }
    let now = Utc::now();
    let galaxies = super::tail::enumerate_galaxies(ctx, args.all_galaxies, None)
        .into_iter()
        .map(|source| seats_for_galaxy(source.label, &source.state_dir, now))
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::to_string_pretty(&SeatsDocument {
            schema_version: 1,
            galaxies
        })?
    );
    Ok(())
}

fn seats_for_galaxy(
    galaxy: String,
    state_dir: &std::path::Path,
    now: DateTime<Utc>,
) -> GalaxySeats {
    let missions_from_molecules = FileStore::new(state_dir)
        .list_molecules(&MoleculeFilter::default())
        .map(|molecules| {
            molecules
                .into_iter()
                .filter(|m| m.tags.iter().any(|tag| tag.as_str() == "pilot-mission"))
                .map(|m| m.id)
                .collect::<std::collections::BTreeSet<_>>()
        });
    let leases = presence::leases_at(state_dir);
    let (Ok(mut missions), Ok(leases)) = (missions_from_molecules, leases) else {
        return GalaxySeats {
            galaxy,
            state: "unreadable",
            missions: Vec::new(),
        };
    };
    let trust_root = leases.trusted_key_id().is_some();
    match leases.missions() {
        Ok(ledger_missions) => missions.extend(ledger_missions),
        Err(_) => {
            return GalaxySeats {
                galaxy,
                state: "unreadable",
                missions: Vec::new(),
            }
        }
    }
    if missions.is_empty() {
        return GalaxySeats {
            galaxy,
            state: "no mission",
            missions: Vec::new(),
        };
    }
    let state = if missions.len() > 1 {
        "ambiguous mission"
    } else if !trust_root {
        "no trust root"
    } else {
        "ok"
    };
    let Ok(presences) = PresenceStore::new(state_dir).scan() else {
        return GalaxySeats {
            galaxy,
            state: "unreadable",
            missions: Vec::new(),
        };
    };
    let mut findings = Vec::with_capacity(missions.len());
    for mission in missions {
        let Some(finding) = seats_for_mission(&leases, &mission, &presences, now) else {
            return GalaxySeats {
                galaxy,
                state: "unreadable",
                missions: Vec::new(),
            };
        };
        findings.push(finding);
    }
    GalaxySeats {
        galaxy,
        state,
        missions: findings,
    }
}

fn seats_for_mission(
    leases: &cosmon_filestore::PilotLeaseStore,
    mission: &MoleculeId,
    presences: &[Presence],
    now: DateTime<Utc>,
) -> Option<MissionSeats> {
    let claims = presences
        .iter()
        .filter(|p| p.mission.as_ref() == Some(mission))
        .collect::<Vec<_>>();
    let audit = leases.audit(mission).ok()?;
    let verified = audit
        .into_iter()
        .filter(|grant| grant.verdict.is_ok())
        .map(|grant| grant.lease)
        .collect::<Vec<_>>();
    let highest = verified.iter().map(|lease| lease.epoch).max();
    let holders = highest
        .map(|epoch| {
            verified
                .iter()
                .filter(|lease| lease.epoch == epoch)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let conflict = holders.len() > 1
        && holders
            .iter()
            .map(|lease| &lease.holder_session_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1;
    let holder = (!conflict).then(|| holders.first().copied()).flatten();
    // A primary claim below the verified lease epoch was valid when it was
    // made and has since been overtaken by a later grant: the ledger already
    // says it no longer holds the seat, so it is reported as `superseded`
    // and is not a competing claim. Only a claim the lease cannot back
    // (same or higher epoch, no epoch, another session) can contest.
    let superseded = |p: &Presence| {
        holder.is_some_and(|lease| {
            p.role.is_primary() && p.lease_epoch.is_some_and(|epoch| epoch < lease.epoch)
        })
    };
    let primary_claims = claims
        .iter()
        .filter(|p| p.role.is_primary() && !superseded(p))
        .count();
    let holder_seated = holder.is_some_and(|lease| {
        claims.iter().any(|p| {
            p.role.is_primary()
                && p.session_id == lease.holder_session_id
                && p.lease_epoch == Some(lease.epoch)
                && !(p.provider.as_deref() == Some("codex") && p.native_session_id.is_none())
        })
    });
    let verdict = if conflict {
        "epoch conflict"
    } else if holder.is_none() {
        "none"
    } else if primary_claims > 1 {
        "contested"
    } else if !holder_seated {
        "granted-unseated"
    } else {
        "one"
    };
    let mut seats = claims
        .into_iter()
        .map(|presence| {
            let matching = holder.is_some_and(|lease| {
                presence.role.is_primary()
                    && presence.session_id == lease.holder_session_id
                    && presence.lease_epoch == Some(lease.epoch)
                    && !(presence.provider.as_deref() == Some("codex")
                        && presence.native_session_id.is_none())
            });
            let standing = if matching {
                "holder"
            } else if superseded(presence) {
                "superseded"
            } else if presence.role.is_primary() {
                "claimant"
            } else {
                "copilot"
            };
            SeatRow {
                session: presence.session_id.to_string(),
                provider: presence.provider.clone(),
                native_id: presence.native_session_id.clone(),
                claimed_role: presence.role.as_str(),
                epoch: presence.lease_epoch.map(LeaseEpoch::get),
                liveness: presence.is_live(now),
                heartbeat: presence.heartbeat_at,
                standing,
            }
        })
        .collect::<Vec<_>>();
    seats.sort_by(|a, b| a.session.cmp(&b.session));
    Some(MissionSeats {
        mission: mission.to_string(),
        verdict,
        seats,
    })
}

fn run_attach(ctx: &Context, args: &AttachArgs) -> anyhow::Result<()> {
    // `--as claude:abc` and `--provider claude --native-session-id abc` are the
    // same claim written two ways; taking both would let them disagree.
    let (provider, native) = match (&args.as_selector, &args.provider, &args.native_session_id) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(anyhow::anyhow!(
                "--as names the same thing as --provider/--native-session-id — pass one form"
            ))
        }
        (Some(raw), None, None) => {
            let sel: SessionSelector = raw.parse()?;
            (
                Some(sel.provider.to_string()),
                Some(sel.native_session_id.to_string()),
            )
        }
        (None, p, n) => (p.clone(), n.clone()),
    };

    // A `follows` that names nobody is a relation to nothing. Resolve it
    // against the registry so the error names the live pilots instead of
    // landing a snapshot that points at a session that never existed.
    let follows = match &args.follow {
        Some(raw) => Some(resolve_pilot_sid(ctx, raw)?),
        None => None,
    };

    let ping_args = presence::PingArgs {
        session: args.session.clone(),
        headline: args.headline.clone(),
        molecule: None,
        galaxy: args.galaxy.clone(),
        provider,
        native_session_id: native,
        role: Some(args.role.clone()),
        follows: follows.as_ref().map(|s| s.as_str().to_owned()),
        capabilities: args.capabilities.clone(),
        checkpoint: None,
        mission: args.mission.clone(),
        epoch: args.epoch,
        state: None,
        lifecycle_source: None,
        lifecycle_sequence: None,
        clear_state: false,
        detail: presence::PresenceDetailUpdate::Preserve,
    };
    let presence = presence::ping(ctx, &ping_args)?;

    if ctx.json {
        println!("{}", serde_json::to_value(&presence)?);
        return Ok(());
    }
    println!(
        "attached {sid} as {role}{selector}{follows}",
        sid = presence.session_id.as_str(),
        role = presence.role.as_str(),
        selector = presence
            .selector()
            .map_or_else(String::new, |s| format!(" [{s}]")),
        follows = presence
            .follows
            .as_ref()
            .map_or_else(String::new, |f| format!(" following {}", f.as_str())),
    );
    Ok(())
}

/// How a peer relates to the session asking.
///
/// Reciprocity is the point of M2's `follows` field, so it is a rendered
/// column rather than something the operator reconstructs by eye.
fn relation(me: &SessionId, peer: &Presence, my_follows: Option<&SessionId>) -> &'static str {
    if &peer.session_id == me {
        return "self";
    }
    let follows_me = peer.follows.as_ref().is_some_and(|f| f == me);
    let i_follow = my_follows.is_some_and(|f| f == &peer.session_id);
    match (follows_me, i_follow) {
        (true, true) => "mutual",
        (true, false) => "follows-me",
        (false, true) => "i-follow",
        (false, false) => "peer",
    }
}

fn run_peers(ctx: &Context, args: &PeersArgs) -> anyhow::Result<()> {
    let me = presence::resolve_or_derive_sid(args.session.as_deref())?;
    let now = Utc::now();
    let mut rows = presence::store(ctx).scan()?;
    if !args.all {
        rows.retain(|p| p.is_live(now));
    }
    rows.sort_by_key(|p| std::cmp::Reverse(p.heartbeat_at));
    let my_follows = rows
        .iter()
        .find(|p| p.session_id == me)
        .and_then(|p| p.follows.clone());

    if ctx.json {
        let mut out = std::io::stdout().lock();
        for p in &rows {
            let mut v = serde_json::to_value(p)?;
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "relation".to_owned(),
                    serde_json::Value::from(relation(&me, p, my_follows.as_ref())),
                );
            }
            writeln!(out, "{v}")?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no pilots present — {} is alone)", me.as_str());
        return Ok(());
    }
    println!(
        "{:<12}  {:<24}  {:<8}  {:<20}  AGE",
        "RELATION", "SESSION", "ROLE", "MISSION",
    );
    for p in &rows {
        println!(
            "{:<12}  {:<24}  {:<8}  {:<20}  {}",
            relation(&me, p, my_follows.as_ref()),
            p.session_id.as_str(),
            p.role.as_str(),
            p.mission.as_ref().map_or("-", MoleculeId::as_str),
            presence::format_age(now, p.heartbeat_at),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// send / inbox — the traced envelope channel
// ---------------------------------------------------------------------------

fn run_send(ctx: &Context, args: &SendArgs) -> anyhow::Result<()> {
    let to = resolve_pilot_sid(ctx, &args.to)?;
    let (message, written) = presence::deliver(
        ctx,
        &presence::SendArgs {
            to: to.as_str().to_owned(),
            message: args.message.clone(),
            from: args.from.clone(),
            expires_in: args.expires_in,
        },
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "id": message.id.as_str(),
                "to": message.to.as_str(),
                "from": message.from.as_str(),
                "sequence": message.sequence,
                "payload_hash": message.payload_hash,
                "delivered": written,
            })
        );
    } else if written {
        println!(
            "sent {id} → {to} (seq {seq})",
            id = message.id,
            to = message.to.as_str(),
            seq = message.sequence,
        );
    } else {
        println!(
            "{id} was already in {to}'s inbox — delivering twice is delivering once",
            id = message.id,
            to = message.to.as_str(),
        );
    }
    Ok(())
}

fn run_inbox(ctx: &Context, args: &InboxArgs) -> anyhow::Result<()> {
    let inbox_args = presence::InboxArgs {
        session: args.session.clone(),
        peek: args.peek,
        all: args.all,
    };
    loop {
        let (sid, rendered) = presence::collect_inbox(ctx, &inbox_args)?;

        if ctx.json {
            let mut out = std::io::stdout().lock();
            for (e, body) in &rendered {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
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
                    })
                )?;
            }
        } else if rendered.is_empty() {
            if !args.follow {
                println!("(no pending messages for {})", sid.as_str());
            }
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

        // Acknowledge only after the text has left this process — a crash now
        // costs a re-read, which is the correct failure (ADR-168 §D4, P3).
        std::io::stdout().flush().ok();
        if !args.peek {
            presence::ack_consumed(ctx, &sid, &rendered)?;
        }

        if !args.follow {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(args.interval.max(1)));
    }
}

// ---------------------------------------------------------------------------
// checkpoint / drift — the hand-over side
// ---------------------------------------------------------------------------

fn checkpoints(ctx: &Context) -> CheckpointStore {
    CheckpointStore::new(presence::state_root(ctx))
}

/// Parse `SUBJECT[:affirm|deny]=STATEMENT` into a claim.
///
/// The subject is the key two pilots must share for their positions to be
/// comparable at all, so it is a required, separate field rather than
/// something inferred from the sentence.
fn parse_claim(id: &str, raw: &str) -> anyhow::Result<Claim> {
    let (head, statement) = raw.split_once('=').ok_or_else(|| {
        anyhow::anyhow!(
            "claim {raw:?} is not `SUBJECT[:affirm|deny]=STATEMENT` — the subject is what \
             makes two pilots' positions comparable, so it cannot be omitted"
        )
    })?;
    let (subject, stance) = match head.split_once(':') {
        Some((s, "affirm")) => (s, Stance::Affirm),
        Some((s, "deny")) => (s, Stance::Deny),
        Some((_, other)) => {
            return Err(anyhow::anyhow!(
                "unknown stance {other:?} in {raw:?} — expected `affirm` or `deny`"
            ))
        }
        None => (head, Stance::Affirm),
    };
    if subject.trim().is_empty() || statement.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "claim {raw:?} has an empty subject or statement"
        ));
    }
    Ok(Claim::new(id, subject.trim(), stance, statement.trim())?)
}

/// Parse `LOCATOR[#DIGEST]` into an evidence reference.
fn parse_evidence(raw: &str) -> EvidenceRef {
    match raw.rsplit_once('#') {
        Some((locator, digest)) if !locator.is_empty() && !digest.is_empty() => {
            EvidenceRef::pinned(locator, digest)
        }
        _ => EvidenceRef::new(raw),
    }
}

/// The only external checkpoint draft shape accepted by `--from-file`.
///
/// This deliberately has no publication identity or authority fields. Those
/// facts come from Cosmon's session and lease stores at publication time, not
/// from bytes supplied by a pilot.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointDraft {
    schema: String,
    #[serde(default)]
    scope: DraftScope,
    #[serde(default)]
    hypotheses: Vec<DraftClaim>,
    #[serde(default)]
    evidence: Vec<DraftEvidenceRef>,
    #[serde(default)]
    completed_actions: Vec<String>,
    #[serde(default)]
    intended_next_actions: Vec<DraftClaim>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    unresolved_questions: Vec<String>,
}

/// JSON-only draft scope, kept separate so unknown external fields fail.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftScope {
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

/// JSON-only draft claim, with the existing checkpoint claim cardinality.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftClaim {
    id: String,
    subject: String,
    stance: Stance,
    statement: String,
    #[serde(default)]
    evidence: Vec<DraftEvidenceRef>,
}

/// JSON-only draft evidence reference.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftEvidenceRef {
    locator: String,
    #[serde(default)]
    digest: Option<String>,
}

impl From<DraftEvidenceRef> for EvidenceRef {
    fn from(value: DraftEvidenceRef) -> Self {
        Self {
            locator: value.locator,
            digest: value.digest,
        }
    }
}

impl TryFrom<DraftClaim> for Claim {
    type Error = cosmon_pilot_checkpoint::CheckpointError;

    fn try_from(value: DraftClaim) -> Result<Self, Self::Error> {
        Ok(
            Claim::new(value.id, value.subject, value.stance, value.statement)?
                .with_evidence(value.evidence.into_iter().map(Into::into)),
        )
    }
}

/// Read and validate a versioned draft from a file or standard input.
fn read_checkpoint_draft(path: &std::path::Path) -> anyhow::Result<CheckpointDraft> {
    let mut bytes = Vec::new();
    if path.as_os_str() == "-" {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(path).map_err(|e| {
            anyhow::anyhow!("could not read checkpoint draft {}: {e}", path.display())
        })?;
    }
    let draft: CheckpointDraft = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("invalid checkpoint draft: {e}"))?;
    if draft.schema != "cosmon.checkpoint-draft/v1" {
        return Err(anyhow::anyhow!(
            "unsupported checkpoint draft schema {:?}; expected \"cosmon.checkpoint-draft/v1\"",
            draft.schema
        ));
    }
    Ok(draft)
}

/// Attach `--evidence SUBJECT=LOCATOR` rows to the claims that carry the
/// subject, in both lists.
///
/// An evidence row whose subject matches no claim is an **error**: silently
/// dropping it would publish a checkpoint the operator believes is cited and
/// which [`cosmon_pilot_checkpoint::DriftClass::MissingEvidence`] will then
/// report against them.
fn attach_evidence(cp: &mut PilotCheckpoint, rows: &[String]) -> anyhow::Result<()> {
    for row in rows {
        let (subject, locator) = row
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("evidence {row:?} is not `SUBJECT=LOCATOR[#DIGEST]`"))?;
        let subject = subject.trim();
        let reference = parse_evidence(locator.trim());
        let mut hit = false;
        for claim in cp
            .current_hypotheses
            .iter_mut()
            .chain(cp.intended_next_actions.iter_mut())
        {
            if claim.subject == subject {
                claim.evidence.push(reference.clone());
                hit = true;
            }
        }
        if !hit {
            return Err(anyhow::anyhow!(
                "evidence {row:?} cites subject {subject:?}, which no claim in this \
                 checkpoint takes a position on"
            ));
        }
    }
    Ok(())
}

/// The default checkpoint id: the publication instant plus a short digest of
/// the publishing session, so two pilots checkpointing in the same second do
/// not collide on one filename.
fn default_checkpoint_id(session: &str, now: DateTime<Utc>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session.as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(6);
    for b in digest.iter().take(3) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!(
        "cp-{}-{:09}-{hex}",
        now.format("%Y%m%dT%H%M%S"),
        now.timestamp_subsec_nanos()
    )
}

fn run_checkpoint(ctx: &Context, sub: &CheckpointSub) -> anyhow::Result<()> {
    match sub {
        CheckpointSub::Publish(a) => run_checkpoint_publish(ctx, a),
        CheckpointSub::Stage(a) => run_checkpoint_stage(ctx, a),
        CheckpointSub::List(a) => run_checkpoint_list(ctx, a),
        CheckpointSub::Show(a) => run_checkpoint_show(ctx, a),
    }
}

/// Build the checkpoint an operator described on the command line.
///
/// Split out of [`run_checkpoint_publish`] so `stage` and `publish` build the
/// *same* record from the *same* flags. A staged checkpoint that differed from
/// a published one in any field would make the hook's transition-time
/// publication a second dialect of the verb it claims to defer.
fn build_checkpoint(
    ctx: &Context,
    args: &CheckpointPublishArgs,
) -> anyhow::Result<PilotCheckpoint> {
    let sid = presence::resolve_sid_for_poll(&presence::PollArgs {
        session: args.session.clone(),
    })?;
    let now = Utc::now();

    if let Some(path) = &args.from_file {
        let draft = read_checkpoint_draft(path)?;
        let mission = MissionId::new(args.mission.clone())?;
        let lease = presence::leases(ctx)?.current(&MoleculeId::new(args.mission.clone())?)?;
        let Some(lease) = lease else {
            return Err(anyhow::anyhow!(
                "cannot publish checkpoint draft: mission {mission} has no verified lease"
            ));
        };
        if lease.holder_session_id.as_str() != sid.as_str() || !lease.is_valid_at(now) {
            return Err(anyhow::anyhow!(
                "cannot publish checkpoint draft: session {sid} is not the current verified lease holder"
            ));
        }
        let id = default_checkpoint_id(sid.as_str(), now);
        let mut cp =
            PilotCheckpoint::new(id, mission.as_str(), sid.as_str(), lease.epoch.get(), now)?;
        cp.scope = Scope::new(draft.scope.includes, draft.scope.excludes);
        cp.current_hypotheses = draft
            .hypotheses
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()?;
        cp.evidence_refs = draft.evidence.into_iter().map(Into::into).collect();
        cp.completed_actions = draft.completed_actions;
        cp.intended_next_actions = draft
            .intended_next_actions
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, _>>()?;
        cp.open_risks = draft.risks;
        cp.unresolved_questions = draft.unresolved_questions;
        return Ok(cp);
    }

    // The epoch a checkpoint records is what the publisher *believed* it held.
    // Reading it off the pilot's own presence snapshot is what makes the
    // common case correct without the operator retyping a number the ledger
    // already knows.
    let epoch = match args.epoch {
        Some(n) => n,
        None => presence::store(ctx)
            .scan()?
            .into_iter()
            .find(|p| p.session_id == sid)
            .and_then(|p| p.lease_epoch)
            .map_or(0, LeaseEpoch::get),
    };

    let id = args
        .id
        .clone()
        .unwrap_or_else(|| default_checkpoint_id(sid.as_str(), now));
    let mut cp = PilotCheckpoint::new(id, args.mission.clone(), sid.as_str(), epoch, now)?;
    cp.scope = Scope::new(args.includes.clone(), args.excludes.clone());
    for (i, raw) in args.hypotheses.iter().enumerate() {
        cp.current_hypotheses
            .push(parse_claim(&format!("h{}", i + 1), raw)?);
    }
    for (i, raw) in args.next_actions.iter().enumerate() {
        cp.intended_next_actions
            .push(parse_claim(&format!("n{}", i + 1), raw)?);
    }
    cp.completed_actions.clone_from(&args.completed);
    cp.open_risks.clone_from(&args.risks);
    cp.unresolved_questions.clone_from(&args.questions);
    cp.evidence_refs = args
        .checkpoint_evidence
        .iter()
        .map(|r| parse_evidence(r))
        .collect();
    attach_evidence(&mut cp, &args.evidence)?;
    Ok(cp)
}

fn run_checkpoint_publish(ctx: &Context, args: &CheckpointPublishArgs) -> anyhow::Result<()> {
    let cp = build_checkpoint(ctx, args)?;
    let path = checkpoints(ctx).publish(&cp)?;

    if ctx.json {
        let mut v = serde_json::to_value(&cp)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "path".to_owned(),
                serde_json::Value::from(path.display().to_string()),
            );
        }
        println!("{v}");
    } else {
        println!(
            "published {id} for {mission} by {sid} at epoch {epoch} → {path}",
            id = cp.id,
            mission = cp.mission_id,
            sid = cp.session_id,
            epoch = cp.lease_epoch,
            path = path.display(),
        );
    }
    Ok(())
}

/// `cs sessions checkpoint stage` — write the record, publish it later.
///
/// The division of labour M6 rests on: a hand-over record's *content* is the
/// pilot's — its hypotheses, its next moves, its open questions — and only the
/// *moment* belongs to the hook. Staging is how a pilot says what it would
/// hand over without having to be awake at the transition to say it.
///
/// One draft per session, overwritten: a queue of stale drafts published at
/// later transitions would be a hand-over record of a mind that has moved on.
fn run_checkpoint_stage(ctx: &Context, args: &CheckpointPublishArgs) -> anyhow::Result<()> {
    let cp = build_checkpoint(ctx, args)?;
    let path = super::sessions_hook::stage(ctx, &cp)?;

    if ctx.json {
        let mut v = serde_json::to_value(&cp)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("staged".to_owned(), serde_json::Value::Bool(true));
            obj.insert(
                "path".to_owned(),
                serde_json::Value::from(path.display().to_string()),
            );
        }
        println!("{v}");
    } else {
        println!(
            "staged {id} for {mission} — the hook publishes it at the next transition → {path}",
            id = cp.id,
            mission = cp.mission_id,
            path = path.display(),
        );
    }
    Ok(())
}

fn run_checkpoint_list(ctx: &Context, args: &CheckpointListArgs) -> anyhow::Result<()> {
    let mission = MissionId::new(args.mission.clone())?;
    let mut rows = checkpoints(ctx).list(&mission)?;
    if let Some(sid) = &args.session {
        let want = CheckpointSessionId::new(sid.clone())?;
        rows.retain(|c| c.session_id == want);
    }

    if ctx.json {
        let mut out = std::io::stdout().lock();
        for c in &rows {
            writeln!(out, "{}", serde_json::to_value(c)?)?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no checkpoints published for {mission})");
        return Ok(());
    }
    println!(
        "{:<32}  {:<24}  {:>6}  {:>5}  {:>5}  CREATED",
        "ID", "SESSION", "EPOCH", "HYP", "NEXT",
    );
    for c in &rows {
        println!(
            "{:<32}  {:<24}  {:>6}  {:>5}  {:>5}  {}",
            c.id.as_str(),
            c.session_id.as_str(),
            c.lease_epoch,
            c.current_hypotheses.len(),
            c.intended_next_actions.len(),
            c.created_at.to_rfc3339(),
        );
    }
    Ok(())
}

/// Load the checkpoint a `--mission` + (`--id` | `--session`) pair names.
///
/// Naming neither is an error rather than a default: "the latest checkpoint of
/// the mission" would silently compare whichever pilot happened to publish
/// last, which is the ambiguity this surface exists to refuse.
fn load_checkpoint(
    ctx: &Context,
    mission: &MissionId,
    id: Option<&String>,
    session: Option<&String>,
) -> anyhow::Result<Option<PilotCheckpoint>> {
    let store = checkpoints(ctx);
    match (id, session) {
        (Some(raw), _) => {
            let id = cosmon_pilot_checkpoint::CheckpointId::new(raw.clone())?;
            Ok(store.load(mission, &id)?)
        }
        (None, Some(sid)) => {
            let sid = CheckpointSessionId::new(sid.clone())?;
            Ok(store.latest_for(mission, &sid)?)
        }
        (None, None) => Err(anyhow::anyhow!(
            "name the checkpoint: --id <ID>, or --session <SID> for that pilot's latest"
        )),
    }
}

fn run_checkpoint_show(ctx: &Context, args: &CheckpointShowArgs) -> anyhow::Result<()> {
    if args.of_holder && !ctx.json {
        return Err(anyhow::anyhow!(
            "--of-holder is a typed selection; pass --json to preserve its null result"
        ));
    }
    let mission = MissionId::new(args.mission.clone())?;
    let (found, selected_by) = if args.of_holder {
        let lease_mission = MoleculeId::new(args.mission.clone())?;
        let lease = presence::leases(ctx)?.current(&lease_mission)?;
        match lease {
            Some(lease) if lease.is_valid_at(Utc::now()) => {
                let holder = CheckpointSessionId::new(lease.holder_session_id.as_str())?;
                (
                    checkpoints(ctx).latest_for_session_epoch(
                        &mission,
                        &holder,
                        lease.epoch.get(),
                    )?,
                    serde_json::json!({
                        "kind": "verified_lease_holder",
                        "holder_session": holder.as_str(),
                        "epoch": lease.epoch.get(),
                    }),
                )
            }
            _ => (
                None,
                serde_json::json!({
                    "kind": "verified_lease_holder",
                    "holder_session": serde_json::Value::Null,
                    "epoch": serde_json::Value::Null,
                }),
            ),
        }
    } else {
        (
            load_checkpoint(ctx, &mission, args.id.as_ref(), args.session.as_ref())?,
            serde_json::Value::Null,
        )
    };

    if args.of_holder && ctx.json {
        println!(
            "{}",
            serde_json::json!({"checkpoint": found, "selected_by": selected_by})
        );
        return Ok(());
    }

    let Some(cp) = found else {
        return Err(anyhow::anyhow!(
            "no such checkpoint for {mission} — `cs sessions checkpoint list --mission {mission}` \
             prints the ones there are"
        ));
    };

    if ctx.json {
        println!("{}", serde_json::to_value(&cp)?);
        return Ok(());
    }
    println!("{id}  ({mission})", id = cp.id, mission = cp.mission_id);
    println!("  session   {}", cp.session_id);
    println!("  epoch     {}", cp.lease_epoch);
    println!("  created   {}", cp.created_at.to_rfc3339());
    if cp.scope.is_empty() {
        println!("  scope     (undeclared — not the same as agreed)");
    } else {
        for i in &cp.scope.includes {
            println!("  includes  {i}");
        }
        for e in &cp.scope.excludes {
            println!("  excludes  {e}");
        }
    }
    for c in &cp.current_hypotheses {
        println!("  holds     {}", render_claim(c));
    }
    for c in &cp.intended_next_actions {
        println!("  next      {}", render_claim(c));
    }
    for a in &cp.completed_actions {
        println!("  done      {a}");
    }
    for r in &cp.open_risks {
        println!("  risk      {r}");
    }
    for q in &cp.unresolved_questions {
        println!("  question  {q}");
    }
    for e in &cp.evidence_refs {
        println!("  evidence  {}", e.locator);
    }
    Ok(())
}

fn render_claim(c: &Claim) -> String {
    format!(
        "[{subject}] {stance} {statement}{evidence}",
        subject = c.subject,
        stance = match c.stance {
            Stance::Affirm => "affirm",
            Stance::Deny => "deny",
        },
        statement = c.statement,
        evidence = if c.evidence.is_empty() {
            " (no evidence cited)".to_owned()
        } else {
            format!(
                " ← {}",
                c.evidence
                    .iter()
                    .map(|e| e.locator.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    )
}

fn run_drift(ctx: &Context, args: &DriftArgs) -> anyhow::Result<()> {
    if args.checkpoint != "latest" {
        return Err(anyhow::anyhow!(
            "--checkpoint takes only `latest`; name an exact record with \
             --checkpoint-a <ID> / --checkpoint-b <ID>"
        ));
    }
    let mission = MissionId::new(args.mission.clone())?;
    let a = load_checkpoint(
        ctx,
        &mission,
        args.checkpoint_a.as_ref(),
        Some(&args.session_a),
    )?;
    let b = load_checkpoint(
        ctx,
        &mission,
        args.checkpoint_b.as_ref(),
        Some(&args.session_b),
    )?;
    let report = compare(a.as_ref(), b.as_ref(), Utc::now());

    if ctx.json {
        println!("{}", serde_json::to_value(&report)?);
    } else {
        println!(
            "{verdict:?} — {n} record(s) comparing {a} and {b} on {mission}",
            verdict = report.verdict,
            n = report.findings.len(),
            a = args.session_a,
            b = args.session_b,
        );
        for f in &report.findings {
            println!("  [{}] {}", f.class.as_str(), f.detail);
            for cited in &f.cited_claims {
                println!(
                    "      {side:?}: {statement}",
                    side = cited.side,
                    statement = cited.statement,
                );
            }
        }
    }

    // Same convention as `cs diverge` and `cs presence lease check`: a
    // decidable answer is a successful run with a meaningful exit code, and
    // `2` keeps "could not compare" out of the "disagree" bucket.
    std::io::stdout().flush().ok();
    std::process::exit(report.verdict.exit_code());
}

// ---------------------------------------------------------------------------
// takeover — authority, and its supervised transfer
// ---------------------------------------------------------------------------

fn run_takeover(ctx: &Context, sub: &TakeoverSub) -> anyhow::Result<()> {
    match sub {
        TakeoverSub::Show(a) => run_takeover_show(ctx, a),
        TakeoverSub::Request(a) => run_takeover_request(ctx, a),
        TakeoverSub::Grant(a) => run_takeover_grant(ctx, a),
        TakeoverSub::Challenge(a) => run_takeover_challenge(ctx, a),
        TakeoverSub::Trust(a) => run_takeover_trust(ctx, a),
        TakeoverSub::Check(a) => run_takeover_check(ctx, a),
    }
}

fn run_takeover_show(ctx: &Context, args: &TakeoverShowArgs) -> anyhow::Result<()> {
    let store = presence::leases(ctx)?;
    let now = Utc::now();
    let current = store.current(&args.mission)?;
    let pending = store.unanswered_requests(&args.mission)?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "mission": args.mission.as_str(),
                "lease": current,
                "valid_now": current.as_ref().is_some_and(|l| l.is_valid_at(now)),
                "next_epoch": store.next_epoch(&args.mission)?,
                "unanswered_requests": pending,
                "trusted_operator_key": store.trusted_key_id().map(|k| k.to_string()),
                "history": if args.history {
                    Some(
                        store
                            .audit(&args.mission)?
                            .into_iter()
                            .map(|g| serde_json::json!({
                                "grant": g.lease,
                                "attested": g.verdict.is_ok(),
                                "refusal": g.verdict.err().map(|e| e.to_string()),
                            }))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                },
            })
        );
        return Ok(());
    }

    match &current {
        None => println!(
            "{mission}: nobody holds the controls — every pilot is read-only",
            mission = args.mission.as_str(),
        ),
        Some(l) => println!(
            "{mission}: {holder} is PRIMARY at epoch {epoch}{validity} (granted by {by})",
            mission = args.mission.as_str(),
            holder = l.holder_session_id.as_str(),
            epoch = l.epoch,
            validity = if l.is_valid_at(now) {
                String::new()
            } else {
                " (EXPIRED)".to_owned()
            },
            by = l.granted_by,
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
        // Refused lines are printed, not hidden. A forged grant that vanishes
        // from every view is a forgery nobody investigates.
        for g in store.audit(&args.mission)? {
            let l = &g.lease;
            println!(
                "  epoch {epoch}: {holder} (by {by} at {at}){verdict}",
                epoch = l.epoch,
                holder = l.holder_session_id.as_str(),
                by = l.granted_by,
                at = l.granted_at,
                verdict = match &g.verdict {
                    Ok(()) => l
                        .attestation
                        .as_ref()
                        .map_or_else(String::new, |a| format!(" — signed by {}", a.key_id),),
                    Err(e) => format!(" — NOT AN OPERATOR GESTURE: {e}"),
                },
            );
        }
    }
    Ok(())
}

/// `cs sessions takeover challenge` — print the bytes, sign them elsewhere.
///
/// Printing a challenge confers nothing, so this verb needs no guard: it is a
/// description of a transfer that has not happened. What makes the transfer
/// happen is a signature over these bytes, and this command deliberately
/// cannot produce one — see [`cosmon_core::operator_attestation`].
fn run_takeover_challenge(ctx: &Context, args: &TakeoverChallengeArgs) -> anyhow::Result<()> {
    let store = presence::leases(ctx)?;
    let holder = match (&args.request, &args.to) {
        (Some(raw), _) => {
            let id = RequestId::new(raw.clone())?;
            store
                .find_request(&args.mission, &id)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no request {id} on mission {mission}",
                        mission = args.mission.as_str()
                    )
                })?
                .candidate_session_id
        }
        (None, Some(sid)) => SessionId::new(sid.clone())?,
        (None, None) => {
            return Err(anyhow::anyhow!(
                "nothing to describe — pass --request <REQUEST_ID> or --to <SID>"
            ))
        }
    };

    let challenge = GrantChallenge::new(
        args.mission.clone(),
        holder,
        store.next_epoch(&args.mission)?,
        presence::resolve_operator_name(args.granted_by.as_deref()),
        args.ttl,
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "challenge": challenge,
                "bytes": challenge.to_string(),
            })
        );
    } else {
        // Bare on stdout, so `cs … challenge … > f && minisign -Sm f` works.
        print!("{challenge}");
    }
    Ok(())
}

/// `cs sessions takeover trust` — which key may seat a pilot here.
fn run_takeover_trust(ctx: &Context, _args: &TakeoverTrustArgs) -> anyhow::Result<()> {
    let resolved = MinisignOperatorVerifier::resolve_for_state_root(presence::state_root(ctx))?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "trusted_key_id": resolved.as_ref().map(|v| v.trusted_key_id().to_string()),
                "source": resolved.as_ref().map(|v| v.source().display().to_string()),
                "default_path": TAKEOVER_PUBKEY_REL,
                "env_override": TAKEOVER_PUBKEY_ENV,
            })
        );
        return Ok(());
    }

    match resolved {
        Some(v) => println!(
            "operator key {id} — read from {src}\n  \
             a grant is honoured only if this key signed it",
            id = v.trusted_key_id(),
            src = v.source().display(),
        ),
        None => println!(
            "no operator key pinned — no transfer can be authorised here.\n  \
             `minisign -G -p {TAKEOVER_PUBKEY_REL} -s <secret>` then commit \
             {TAKEOVER_PUBKEY_REL},\n  \
             so a swapped trust root is a diff somebody reads. Or set \
             ${TAKEOVER_PUBKEY_ENV} to a path."
        ),
    }
    Ok(())
}

fn run_takeover_request(ctx: &Context, args: &TakeoverRequestArgs) -> anyhow::Result<()> {
    let (request, written) = presence::request_lease(
        ctx,
        &presence::LeaseRequestArgs {
            mission: args.mission.clone(),
            to: args.to.clone(),
            from: args.from.clone(),
            reason: args.reason.clone(),
        },
    )?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "request": request,
                "recorded": written,
                "authority_changed": false,
            })
        );
    } else if written {
        println!(
            "requested {id} — it confers nothing until an operator runs \
             `cs sessions takeover grant --mission {mission} --request {id}`",
            id = request.id,
            mission = args.mission.as_str(),
        );
    } else {
        println!(
            "{id} was already requested — asking twice is asking once",
            id = request.id,
        );
    }
    Ok(())
}

fn run_takeover_grant(ctx: &Context, args: &TakeoverGrantArgs) -> anyhow::Result<()> {
    let lease = presence::grant_lease(
        ctx,
        &presence::LeaseGrantArgs {
            mission: args.mission.clone(),
            request: args.request.clone(),
            to: args.to.clone(),
            ttl: args.ttl,
            granted_by: args.granted_by.clone(),
            attestation: args.attestation.clone(),
            sign_with: args.sign_with.clone(),
        },
    )?;

    if ctx.json {
        println!("{}", serde_json::to_string(&lease)?);
    } else {
        println!(
            "{mission}: {holder} is PRIMARY at epoch {epoch} — every earlier epoch is refused",
            mission = args.mission.as_str(),
            holder = lease.holder_session_id.as_str(),
            epoch = lease.epoch,
        );
    }
    Ok(())
}

fn run_takeover_check(ctx: &Context, args: &TakeoverCheckArgs) -> anyhow::Result<()> {
    let session = presence::resolve_sid_for_poll(&presence::PollArgs {
        session: args.session.clone(),
    })?;
    let epoch = match args.epoch {
        Some(raw) => Some(LeaseEpoch::new(raw)?),
        None => None,
    };
    let decision = presence::leases(ctx)?.authorize(&args.mission, Utc::now(), &session, epoch)?;

    // What this session's *seat* would present, which is what a lifecycle verb
    // actually carries — `--epoch` is what the caller asserts, and the two can
    // disagree. They did in the M8 relève exercise: the successor was told
    // `granted` while its snapshot still said `role: copilot`, so its seat
    // presented nothing and its first real gesture would have been refused. A
    // confirmation that can be right about the ledger and wrong about the
    // gesture is worse than no confirmation, because a relève consults it at
    // exactly the moment nobody can afford to re-check by hand.
    let caveat = presence::seat_caveat(ctx, &session, &args.mission, epoch, &decision)?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "decision": decision,
                "seat_would_be_refused": caveat.is_some(),
            })
        );
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
                why = RefusalReason::explain(reason),
            ),
        }
        if let Some(line) = caveat {
            println!("{line}");
        }
    }
    std::io::stdout().flush().ok();
    std::process::exit(i32::from(!decision.is_granted()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use cosmon_core::operator_attestation::{AttestationError, OperatorAttestation, OperatorKeyId};
    use cosmon_core::pilot_lease::PilotLease;

    use std::path::Path;
    use tempfile::tempdir;

    #[derive(Debug)]
    struct FixtureVerifier;

    impl OperatorGestureVerifier for FixtureVerifier {
        fn verify(
            &self,
            _: &GrantChallenge,
            _: &OperatorAttestation,
        ) -> Result<(), AttestationError> {
            Ok(())
        }

        fn trusted_key_id(&self) -> OperatorKeyId {
            OperatorKeyId::from_bytes([1; 8])
        }
    }

    fn ctx_for(dir: &Path) -> Context {
        Context {
            verbose: false,
            json: false,
            config: Some(dir.to_path_buf()),
        }
    }

    #[test]
    fn seats_reports_no_mission_as_readable_data() {
        let dir = tempdir().unwrap();
        let finding = seats_for_galaxy("fixture".to_owned(), dir.path(), Utc::now());
        assert_eq!(finding.state, "no mission");
        assert!(finding.missions.is_empty());
    }

    fn fixture_mission() -> MoleculeId {
        MoleculeId::new("task-20261008-seat").unwrap()
    }

    fn fixture_lease(holder: &str, epoch: u64) -> PilotLease {
        let mut lease = PilotLease::new(
            fixture_mission(),
            SessionId::new(holder).unwrap(),
            LeaseEpoch::new(epoch).unwrap(),
            "operator",
            Utc::now(),
            None,
        );
        lease.attestation = Some(OperatorAttestation {
            key_id: OperatorKeyId::from_bytes([1; 8]),
            signature: "fixture".to_owned(),
            global_signature: "fixture".to_owned(),
            trusted_comment: "fixture".to_owned(),
            untrusted_comment: String::new(),
        });
        lease
    }

    fn fixture_store(dir: &Path) -> cosmon_filestore::PilotLeaseStore {
        cosmon_filestore::PilotLeaseStore::new(dir).trusting(Arc::new(FixtureVerifier))
    }

    fn primary(session: &str, epoch: u64, heartbeat: DateTime<Utc>) -> Presence {
        let mut presence = Presence::new(
            SessionId::new(session).unwrap(),
            "fixture",
            "/tmp",
            0,
            heartbeat,
        );
        presence.role = cosmon_core::presence::PilotRole::Primary;
        presence.mission = Some(fixture_mission());
        presence.lease_epoch = Some(LeaseEpoch::new(epoch).unwrap());
        presence
    }

    #[test]
    fn seats_verdicts_keep_ledger_authority_over_fresh_claims() {
        let dir = tempdir().unwrap();
        let store = fixture_store(dir.path());
        store.grant(&fixture_lease("old", 1)).unwrap();
        store.grant(&fixture_lease("current", 2)).unwrap();
        let old = primary("old", 1, Utc::now());
        let current = primary("current", 2, Utc::now() - chrono::Duration::minutes(1));
        // Transfer window: the incoming holder is seated at epoch 2 while the
        // outgoing presence still claims primary at epoch 1. Only one claim is
        // backed by the lease, so this is not a contest.
        let transfer =
            seats_for_mission(&store, &fixture_mission(), &[old, current], Utc::now()).unwrap();
        assert_eq!(transfer.verdict, "one");
        let standing = |finding: &MissionSeats, session: &str| {
            finding
                .seats
                .iter()
                .find(|seat| seat.session == session)
                .unwrap()
                .standing
        };
        assert_eq!(standing(&transfer, "current"), "holder");
        assert_eq!(standing(&transfer, "old"), "superseded");
        let one = seats_for_mission(
            &store,
            &fixture_mission(),
            &[primary("current", 2, Utc::now())],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(one.verdict, "one");
        assert!(one.seats[0].liveness);

        let empty = seats_for_mission(
            &fixture_store(tempdir().unwrap().path()),
            &fixture_mission(),
            &[],
            Utc::now(),
        );
        assert_eq!(empty.unwrap().verdict, "none");
        let unseated = seats_for_mission(&store, &fixture_mission(), &[], Utc::now()).unwrap();
        assert_eq!(unseated.verdict, "granted-unseated");
    }

    #[test]
    fn transfer_sequence_never_reads_contested() {
        let dir = tempdir().unwrap();
        let store = fixture_store(dir.path());
        let now = Utc::now();
        let verdict = |presences: &[Presence]| {
            seats_for_mission(&store, &fixture_mission(), presences, now)
                .unwrap()
                .verdict
        };
        store.grant(&fixture_lease("outgoing", 1)).unwrap();
        assert_eq!(verdict(&[primary("outgoing", 1, now)]), "one");
        // Incoming granted epoch 2, not yet seated.
        store.grant(&fixture_lease("incoming", 2)).unwrap();
        assert_eq!(verdict(&[primary("outgoing", 1, now)]), "granted-unseated");
        // Incoming seated; outgoing has not yet beaten again.
        assert_eq!(
            verdict(&[primary("outgoing", 1, now), primary("incoming", 2, now)]),
            "one"
        );
        // Outgoing demoted itself to copilot.
        let mut demoted = primary("outgoing", 1, now);
        demoted.role = cosmon_core::presence::PilotRole::Copilot;
        assert_eq!(verdict(&[demoted, primary("incoming", 2, now)]), "one");
    }

    #[test]
    fn real_conflicts_stay_contested() {
        let dir = tempdir().unwrap();
        let store = fixture_store(dir.path());
        let now = Utc::now();
        store.grant(&fixture_lease("current", 2)).unwrap();
        let verdict = |presences: &[Presence]| {
            seats_for_mission(&store, &fixture_mission(), presences, now)
                .unwrap()
                .verdict
        };
        // Another session claims primary at the lease epoch.
        assert_eq!(
            verdict(&[primary("current", 2, now), primary("rival", 2, now)]),
            "contested"
        );
        // A claim above any verified epoch cannot be backed by the lease.
        assert_eq!(
            verdict(&[primary("current", 2, now), primary("rival", 3, now)]),
            "contested"
        );
        // A primary claim carrying no epoch at all is unverifiable.
        let mut unverifiable = primary("rival", 2, now);
        unverifiable.lease_epoch = None;
        assert_eq!(
            verdict(&[primary("current", 2, now), unverifiable]),
            "contested"
        );
    }

    #[test]
    fn codex_without_native_id_cannot_stand_as_holder() {
        let dir = tempdir().unwrap();
        let store = fixture_store(dir.path());
        store.grant(&fixture_lease("codex", 1)).unwrap();
        let mut claimant = primary("codex", 1, Utc::now());
        claimant.provider = Some("codex".to_owned());
        let finding =
            seats_for_mission(&store, &fixture_mission(), &[claimant], Utc::now()).unwrap();
        assert_eq!(finding.verdict, "granted-unseated");
        assert_eq!(finding.seats[0].standing, "claimant");
    }

    #[test]
    fn duplicate_highest_epoch_is_an_epoch_conflict() {
        let dir = tempdir().unwrap();
        let store = fixture_store(dir.path());
        let first = fixture_lease("first", 1);
        let second = fixture_lease("second", 1);
        std::fs::create_dir_all(store.grants_path(&fixture_mission()).parent().unwrap()).unwrap();
        std::fs::write(
            store.grants_path(&fixture_mission()),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&second).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            seats_for_mission(&store, &fixture_mission(), &[], Utc::now())
                .unwrap()
                .verdict,
            "epoch conflict"
        );
    }

    #[test]
    fn seats_requires_global_json() {
        let dir = tempdir().unwrap();
        let err = run_seats(&ctx_for(dir.path()), &SeatsArgs::default()).unwrap_err();
        assert!(err.to_string().contains("--json"));
    }

    #[test]
    fn seats_distinguishes_every_galaxy_state() {
        let empty = tempdir().unwrap();
        assert_eq!(
            seats_for_galaxy("empty".to_owned(), empty.path(), Utc::now()).state,
            "no mission"
        );

        let no_root = tempdir().unwrap();
        std::fs::create_dir_all(no_root.path().join("pilot-lease")).unwrap();
        std::fs::write(
            no_root
                .path()
                .join("pilot-lease/task-20261008-seat.requests.jsonl"),
            "",
        )
        .unwrap();
        assert_eq!(
            seats_for_galaxy("no-root".to_owned(), no_root.path(), Utc::now()).state,
            "no trust root"
        );

        let trusted_root = tempdir().unwrap();
        let trusted_state = trusted_root.path().join(".cosmon/state");
        std::fs::create_dir_all(trusted_state.join("pilot-lease")).unwrap();
        std::fs::write(
            trusted_root.path().join(".cosmon/takeover.pub"),
            cosmon_minisign_testkit::Operator::from_seed(7).public_key_file(),
        )
        .unwrap();
        std::fs::write(
            trusted_state.join("pilot-lease/task-20261008-seat.requests.jsonl"),
            "",
        )
        .unwrap();
        assert_eq!(
            seats_for_galaxy("trusted".to_owned(), &trusted_state, Utc::now()).state,
            "ok"
        );

        let ambiguous = tempdir().unwrap();
        std::fs::create_dir_all(ambiguous.path().join("pilot-lease")).unwrap();
        std::fs::write(
            ambiguous
                .path()
                .join("pilot-lease/task-20261008-seat.requests.jsonl"),
            "",
        )
        .unwrap();
        std::fs::write(
            ambiguous
                .path()
                .join("pilot-lease/task-20261008-other.requests.jsonl"),
            "",
        )
        .unwrap();
        assert_eq!(
            seats_for_galaxy("ambiguous".to_owned(), ambiguous.path(), Utc::now()).state,
            "ambiguous mission"
        );

        let unreadable = tempdir().unwrap();
        std::fs::write(unreadable.path().join("pilot-lease"), "not a directory").unwrap();
        assert_eq!(
            seats_for_galaxy("broken".to_owned(), unreadable.path(), Utc::now()).state,
            "unreadable"
        );
    }

    /// Write a minimal Codex rollout log the probe can discover, and return
    /// its native session id.
    fn codex_log(root: &Path, native: &str, cwd: &Path) -> String {
        let dir = root.join("2026").join("08").join("03");
        std::fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({
            "timestamp": "2026-08-03T10:00:00.000Z",
            "type": "session_meta",
            "payload": { "session_id": native, "cwd": cwd.display().to_string() },
        });
        std::fs::write(
            dir.join(format!("rollout-2026-08-03T10-00-00-{native}.jsonl")),
            format!("{line}\n"),
        )
        .unwrap();
        native.to_owned()
    }

    #[test]
    fn a_selector_round_trips_from_discovery_into_resolution() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let native = codex_log(
            home.path(),
            "0198aaaa-1111-4000-8000-000000000001",
            repo.path(),
        );

        let reg = ProbeRegistry::new().with(Box::new(CodexProbe::new(home.path()).unwrap()));
        let found = reg.discover(&DiscoveryFilter::all()).unwrap();
        assert_eq!(found.len(), 1, "one log, one session");

        let selector = found[0].selector().to_string();
        assert_eq!(selector, format!("codex:{native}"));

        let resolved = resolve_session(&reg, &selector, &DiscoveryFilter::all()).unwrap();
        assert_eq!(resolved.native_session_id.as_str(), native);
    }

    #[test]
    fn an_unknown_selector_names_the_candidates_and_refuses() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        codex_log(
            home.path(),
            "0198aaaa-1111-4000-8000-000000000002",
            repo.path(),
        );

        let reg = ProbeRegistry::new().with(Box::new(CodexProbe::new(home.path()).unwrap()));
        let err = resolve_session(&reg, "codex:nope", &DiscoveryFilter::all())
            .expect_err("an unknown id must not resolve");
        let text = format!("{err}");
        assert!(text.contains("no session codex:nope"), "{text}");
        assert!(
            text.contains("0198aaaa-1111-4000-8000-000000000002"),
            "the error must offer the ids that do exist: {text}"
        );
    }

    #[test]
    fn a_malformed_selector_says_what_the_shape_is() {
        let reg = ProbeRegistry::new();
        let err = resolve_session(&reg, "just-an-id", &DiscoveryFilter::all()).unwrap_err();
        assert!(
            format!("{err}").contains("<provider>:<native-session-id>"),
            "{err}"
        );
    }

    #[test]
    fn two_sessions_in_one_cwd_stay_two_sessions() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        codex_log(
            home.path(),
            "0198aaaa-1111-4000-8000-00000000000a",
            repo.path(),
        );
        codex_log(
            home.path(),
            "0198aaaa-1111-4000-8000-00000000000b",
            repo.path(),
        );

        let reg = ProbeRegistry::new().with(Box::new(CodexProbe::new(home.path()).unwrap()));
        let found = reg.discover(&DiscoveryFilter::all()).unwrap();
        assert_eq!(found.len(), 2, "no collapsing by working directory");
        // And each is still individually addressable — mission falsifier 3.
        for s in &found {
            let one = resolve_session(&reg, &s.selector().to_string(), &DiscoveryFilter::all());
            assert!(one.is_ok(), "{:?}", one.err());
        }
    }

    #[test]
    fn a_claim_parses_its_subject_stance_and_statement() {
        let c = parse_claim("h1", "merge-strategy:deny=do not merge before the doc gate").unwrap();
        assert_eq!(c.subject, "merge-strategy");
        assert_eq!(c.stance, Stance::Deny);
        assert_eq!(c.statement, "do not merge before the doc gate");

        let default_stance = parse_claim("h2", "cursor-is-byte-offset=it is").unwrap();
        assert_eq!(default_stance.stance, Stance::Affirm);

        assert!(parse_claim("h3", "no-equals-sign").is_err());
        assert!(parse_claim("h4", "subject:maybe=hedged").is_err());
    }

    #[test]
    fn evidence_for_an_uncited_subject_is_refused() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut cp = PilotCheckpoint::new("cp-1", "task-1", "sess-a", 1, now).unwrap();
        cp.current_hypotheses
            .push(parse_claim("h1", "scope=only the port").unwrap());

        attach_evidence(&mut cp, &["scope=docs/adr/168.md".to_owned()]).unwrap();
        assert_eq!(cp.current_hypotheses[0].evidence.len(), 1);

        let err = attach_evidence(&mut cp, &["other=docs/adr/111.md".to_owned()]).unwrap_err();
        assert!(format!("{err}").contains("no claim"), "{err}");
    }

    #[test]
    fn evidence_pins_a_digest_when_one_is_given() {
        assert_eq!(parse_evidence("docs/x.md").digest, None);
        let pinned = parse_evidence("docs/x.md#deadbeef");
        assert_eq!(pinned.locator, "docs/x.md");
        assert_eq!(pinned.digest.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn a_checkpoint_id_is_unique_per_session_within_a_second() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let a = default_checkpoint_id("sess-a", now);
        let b = default_checkpoint_id("sess-b", now);
        assert_ne!(a, b, "two pilots in one second must not collide");
        assert!(a.starts_with("cp-2023"), "{a}");
    }

    #[test]
    fn naming_neither_id_nor_session_refuses_rather_than_guessing() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        let mission = MissionId::new("task-1").unwrap();
        let err = load_checkpoint(&ctx, &mission, None, None).unwrap_err();
        assert!(format!("{err}").contains("--id"), "{err}");
    }

    #[test]
    fn a_pilot_selector_that_nobody_advertises_lists_who_is_present() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        presence::ping(
            &ctx,
            &presence::PingArgs {
                session: Some("sess-claude".to_owned()),
                provider: Some("claude".to_owned()),
                native_session_id: Some("4940f28e".to_owned()),
                galaxy: "cosmon".to_owned(),
                ..presence::PingArgs::default()
            },
        )
        .unwrap();

        // The one that is advertised resolves…
        let hit = resolve_pilot_sid(&ctx, "claude:4940f28e").unwrap();
        assert_eq!(hit.as_str(), "sess-claude");

        // …and the one that is not names the pilots that are.
        let err = resolve_pilot_sid(&ctx, "codex:0198bbbb").unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("no live pilot"), "{text}");
        assert!(text.contains("sess-claude"), "{text}");
    }

    #[test]
    fn a_bare_session_id_is_not_run_through_the_registry() {
        let dir = tempdir().unwrap();
        let ctx = ctx_for(dir.path());
        // No presence file exists at all; a plain id is still usable, because
        // a mailbox may be written before its owner has ever pinged.
        let sid = resolve_pilot_sid(&ctx, "sess-nobody").unwrap();
        assert_eq!(sid.as_str(), "sess-nobody");
    }

    #[test]
    fn relation_reads_both_directions_of_follows() {
        let now = Utc::now();
        let me = SessionId::new("sess-me").unwrap();
        let mut peer = Presence::new(
            SessionId::new("sess-them").unwrap(),
            "cosmon".to_owned(),
            PathBuf::from("/tmp"),
            1,
            now,
        );
        assert_eq!(relation(&me, &peer, None), "peer");

        peer.follows = Some(me.clone());
        assert_eq!(relation(&me, &peer, None), "follows-me");
        assert_eq!(
            relation(&me, &peer, Some(&peer.session_id.clone())),
            "mutual"
        );

        peer.follows = None;
        assert_eq!(
            relation(&me, &peer, Some(&peer.session_id.clone())),
            "i-follow"
        );
    }
}
