// SPDX-License-Identifier: AGPL-3.0-only

//! Provider-neutral CLI boundary for messages in a declared work (ADR-182).
//!
//! This command only reads and writes the owner's `work/` evidence and member
//! reference files. It never calls a molecule lifecycle operation.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use chrono::Utc;
use clap::{Args as ClapArgs, Subcommand};
use cosmon_core::advisory_attempt::AdvisorySeatId;
use cosmon_core::id::MoleculeId;
use cosmon_core::work_message::{
    accept_consumption, deliverable, render_for_context, Admission, Confidentiality, Consumption,
    ContextObservation, DeliveryAdapter, DeliveryOutcome, Disposition, MessageBudget, MessageKey,
    ObserverId, Receipt, SeatDecl, SenderEvidence, Stage, Submission, WorkMessageStore,
    WorkProjection, WorkScope, WORK_MESSAGE_SCHEMA_VERSION,
};
use cosmon_hash::Hash;
use cosmon_state::work_message::FileWorkMessageStore;
use serde::{Deserialize, Serialize};

use super::Context;

/// What `cs work --help` opens with, before the verbs and the examples.
///
/// Printed once, on `cs work --help`; kept as a constant (rather than the
/// doc comment on the `Work` variant in `main.rs`, which is also the one
/// line shown in `cs help`) so the longer explanation does not force that
/// line to wrap.
pub const LONG_ABOUT: &str = "\
Separate molecules — an implementer and a reviewer, say, on the same
provider or on two different ones — exchange findings under a declared,
bounded work scope (ADR-182). Each molecule keeps its own lifecycle,
branch and permissions; it stays visible in `cs peek` and is harvested on
its own. Messages carry evidence and requests only: they never advance a
molecule, satisfy a dependency or grant one molecule authority over
another.

The owning molecule declares a finite seat roster once (`declare`); each
member then sends bounded evidence to another seat (`send`), pulls what is
addressed to it (`inbox`), and reports how it treated a message
(`ack`). `list` rebuilds every message's stage — admitted, delivered,
consumed — from canonical evidence, for any member or for the owner.

For two live sessions on ONE mission instead, where a human signature
moves the controls between them, see `cs sessions`.";

/// A refused work operation, reported with exit code 2 for scripts.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WorkRefusal(String);

fn refuse(message: impl Into<String>) -> anyhow::Error {
    WorkRefusal(message.into()).into()
}

/// Work messaging subcommands.
#[derive(ClapArgs)]
pub struct Args {
    /// Operation within a declared work.
    #[command(subcommand)]
    command: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Declare or revise a finite roster under an owning molecule
    Declare(DeclareArgs),
    /// Admit bounded evidence from this member to another seat
    Send(SendArgs),
    /// Pull messages addressed to this member at a safe point
    Inbox(InboxArgs),
    /// Report how this member treated a received message
    Ack(AckArgs),
    /// Rebuild the owner's message stages from canonical evidence
    List(ListArgs),
}

#[derive(ClapArgs)]
struct DeclareArgs {
    /// Owning molecule ID.
    owner: String,
    /// Seat and molecule, written NAME=MOLECULE; repeat for each member.
    #[arg(long = "seat", required = true, value_name = "NAME=MOLECULE")]
    seats: Vec<String>,
    /// Largest payload admitted, in bytes.
    #[arg(long, default_value_t = 16384)]
    max_payload_bytes: u64,
    /// Maximum messages sent by each seat.
    #[arg(long, default_value_t = 100)]
    max_messages_per_seat: u32,
    /// Maximum aggregate payload bytes sent by each seat.
    #[arg(long, default_value_t = 1_048_576)]
    max_bytes_per_seat: u64,
    /// Default message lifetime, in seconds.
    #[arg(long, default_value_t = 86400)]
    default_ttl_secs: u64,
    /// Minimum seconds before the same adapter offers a message again.
    #[arg(long, default_value_t = 60)]
    redeliver_after_secs: u64,
    /// Maximum delivery attempts by each adapter.
    #[arg(long, default_value_t = 10)]
    max_delivery_attempts: u32,
}

#[derive(ClapArgs)]
struct SendArgs {
    /// Receiving seat.
    #[arg(long)]
    to: String,
    /// Read payload bytes from a file.
    #[arg(long, conflicts_with = "text", required_unless_present = "text")]
    file: Option<PathBuf>,
    /// Use this text as the payload.
    #[arg(long, conflicts_with = "file", required_unless_present = "file")]
    text: Option<String>,
    /// Caller-chosen idempotency key; otherwise derived from sender, recipient and digest.
    #[arg(long)]
    key: Option<String>,
    /// Key of the message being answered.
    #[arg(long)]
    reply_to: Option<String>,
    /// Message lifetime override, in seconds.
    #[arg(long)]
    ttl: Option<u64>,
    /// Free-form phase label for evidence only.
    #[arg(long)]
    phase: Option<String>,
}

#[derive(ClapArgs)]
struct InboxArgs {
    /// Show deliverable messages without recording a delivery attempt.
    #[arg(long)]
    peek: bool,
}

#[derive(ClapArgs)]
struct AckArgs {
    /// Key of the received message.
    key: String,
    /// Report that the message was considered.
    #[arg(long, required_unless_present_any = ["deferred", "rejected"], conflicts_with_all = ["deferred", "rejected"])]
    considered: bool,
    /// Report that the message was deferred.
    #[arg(long, conflicts_with = "rejected")]
    deferred: bool,
    /// Report that the message was rejected.
    #[arg(long)]
    rejected: bool,
    /// Key of an already admitted reply.
    #[arg(long)]
    reply: Option<String>,
    /// Optional consumption note, stored by its digest beside the receipts.
    #[arg(long)]
    note: Option<String>,
}

#[derive(ClapArgs)]
struct ListArgs {
    /// Owner molecule; defaults to the caller's declared work.
    owner: Option<String>,
    /// Only envelopes still pending for a recipient.
    #[arg(long, conflicts_with = "open")]
    pending: bool,
    /// Only envelopes without a consumption report.
    #[arg(long)]
    open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkRef {
    owner_molecule: MoleculeId,
    seat: AdvisorySeatId,
}

struct Member {
    seat: AdvisorySeatId,
    store: FileWorkMessageStore,
    scope: WorkScope,
}

/// Dispatch a work operation without entering the lifecycle port.
///
/// # Errors
/// Returns a refusal, custody error or malformed-record error.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.command {
        Verb::Declare(args) => declare(ctx, args),
        Verb::Send(args) => send(ctx, args),
        Verb::Inbox(args) => inbox(ctx, args),
        Verb::Ack(args) => ack(ctx, args),
        Verb::List(args) => list(ctx, args),
    }
}

fn parse_id(value: &str) -> Result<MoleculeId> {
    MoleculeId::new(value).map_err(|error| refuse(error.to_string()))
}

fn parse_seat(value: &str) -> Result<AdvisorySeatId> {
    AdvisorySeatId::new(value).map_err(|error| refuse(error.to_string()))
}

fn parse_key(value: &str) -> Result<MessageKey> {
    MessageKey::new(value).map_err(|error| refuse(error.to_string()))
}

fn molecule_dir(ctx: &Context, id: &MoleculeId) -> PathBuf {
    ctx.store().molecule_dir(id)
}

fn existing_molecule_dir(ctx: &Context, id: &MoleculeId) -> Result<PathBuf> {
    let dir = molecule_dir(ctx, id);
    if !dir.join("state.json").is_file() {
        return Err(refuse(format!(
            "molecule {id} has no state.json in this galaxy"
        )));
    }
    Ok(dir)
}

fn declare(ctx: &Context, args: &DeclareArgs) -> Result<()> {
    let owner = parse_id(&args.owner)?;
    let owner_dir = existing_molecule_dir(ctx, &owner)?;
    let mut seats = BTreeMap::new();
    for entry in &args.seats {
        let (name, molecule) = entry
            .split_once('=')
            .ok_or_else(|| refuse(format!("seat must be NAME=MOLECULE: {entry}")))?;
        let seat = parse_seat(name)?;
        let molecule = parse_id(molecule)?;
        existing_molecule_dir(ctx, &molecule)?;
        if seats
            .insert(
                seat,
                SeatDecl {
                    molecule,
                    required: true,
                    provider_requirement: None,
                },
            )
            .is_some()
        {
            return Err(refuse(format!("duplicate seat name {name}")));
        }
    }
    let budget = MessageBudget {
        max_payload_bytes: args.max_payload_bytes,
        max_messages_per_seat: args.max_messages_per_seat,
        max_bytes_per_seat: args.max_bytes_per_seat,
        default_ttl_secs: args.default_ttl_secs,
        redeliver_after_secs: args.redeliver_after_secs,
        max_delivery_attempts: args.max_delivery_attempts,
    };
    let store = FileWorkMessageStore::new(owner_dir);
    let existing = store.load_scope()?;
    if existing.as_ref().is_some_and(|prior| prior.owner != owner) {
        return Err(refuse("existing work scope names a different owner"));
    }
    let (scope, unchanged) = match existing {
        Some(previous) if previous.seats == seats && previous.budget == budget => (previous, true),
        _ => (
            WorkScope {
                schema_version: WORK_MESSAGE_SCHEMA_VERSION,
                owner: owner.clone(),
                seats,
                budget,
                declared_at: Utc::now(),
            },
            false,
        ),
    };
    scope
        .validate()
        .map_err(|error| refuse(error.to_string()))?;
    // Refuse a conflicting member reference before publishing a new scope.
    for (seat, decl) in &scope.seats {
        let path = molecule_dir(ctx, &decl.molecule).join("work-ref.json");
        if path.exists() {
            let prior: WorkRef = serde_json::from_slice(&fs::read(&path)?)?;
            if prior.owner_molecule != owner || prior.seat != *seat {
                return Err(refuse(format!(
                    "molecule {} already belongs to another work seat",
                    decl.molecule
                )));
            }
        }
    }
    let revision = if unchanged {
        scope
            .revision()
            .map_err(|error| refuse(error.to_string()))?
    } else {
        store.declare(&scope)?
    };
    for (seat, decl) in &scope.seats {
        let path = molecule_dir(ctx, &decl.molecule).join("work-ref.json");
        if !path.exists() {
            write_private_ref(
                &path,
                &WorkRef {
                    owner_molecule: owner.clone(),
                    seat: seat.clone(),
                },
            )?;
        }
    }
    println!(
        "work {owner} revision {revision}; {} seats",
        scope.seats.len()
    );
    Ok(())
}

fn write_private_ref(path: &Path, reference: &WorkRef) -> Result<()> {
    let bytes = serde_json::to_vec(reference)?;
    let parent = path
        .parent()
        .ok_or_else(|| refuse("work reference has no parent"))?;
    let pending = parent.join(format!(
        ".work-ref-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&pending)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::hard_link(&pending, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(pending);
    result
}

fn lock_work(ctx: &Context, owner: &MoleculeId) -> Result<fs::File> {
    let path = molecule_dir(ctx, owner).join("work/work.lock");
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

fn caller_member(ctx: &Context) -> Result<Member> {
    let supplied = std::env::var_os("COSMON_MOL_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| refuse("COSMON_MOL_DIR is required for member operations"))?;
    let name = supplied
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refuse("COSMON_MOL_DIR does not name a molecule"))?;
    let molecule = parse_id(name)?;
    let expected = existing_molecule_dir(ctx, &molecule)?;
    if fs::canonicalize(&supplied)? != fs::canonicalize(&expected)? {
        return Err(refuse(
            "COSMON_MOL_DIR is outside this galaxy's molecule roster",
        ));
    }
    let reference: WorkRef = serde_json::from_slice(
        &fs::read(expected.join("work-ref.json"))
            .map_err(|_| refuse("caller has no declared work-ref.json"))?,
    )?;
    let store = FileWorkMessageStore::new(existing_molecule_dir(ctx, &reference.owner_molecule)?);
    let scope = store
        .load_scope()?
        .ok_or_else(|| refuse("work scope is not declared"))?;
    if scope.owner != reference.owner_molecule || scope.seat_of(&molecule) != Some(&reference.seat)
    {
        return Err(refuse(
            "caller is not a member of this work's current roster",
        ));
    }
    Ok(Member {
        seat: reference.seat,
        store,
        scope,
    })
}

fn send(ctx: &Context, args: &SendArgs) -> Result<()> {
    let member = caller_member(ctx)?;
    let bytes = match (&args.file, &args.text) {
        (Some(path), None) => {
            fs::read(path).with_context(|| format!("read payload {}", path.display()))?
        }
        (None, Some(text)) => text.as_bytes().to_vec(),
        _ => return Err(refuse("choose exactly one of --file or --text")),
    };
    let now = Utc::now();
    let submission = Submission {
        scope_revision: member
            .scope
            .revision()
            .map_err(|error| refuse(error.to_string()))?,
        sender: member.seat,
        recipient: parse_seat(&args.to)?,
        key: args.key.as_deref().map(parse_key).transpose()?,
        payload_digest: Hash::of_bytes(&bytes),
        payload_bytes: bytes.len() as u64,
        sender_time: now,
        reply_to: args.reply_to.as_deref().map(parse_key).transpose()?,
        phase: args.phase.clone(),
        confidentiality: Confidentiality::Internal,
        ttl_secs: args.ttl,
        sender_evidence: SenderEvidence::CallerEnvSameUid,
    };
    let result = member
        .store
        .submit(submission, &bytes, now)
        .map_err(|error| refuse(error.to_string()))?;
    let (kind, envelope) = match result {
        Admission::Admit(envelope) => ("admitted", envelope),
        Admission::Duplicate(envelope) => ("duplicate", envelope),
    };
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({"status": kind, "key": envelope.key, "digest": envelope.payload_digest})
        );
    } else {
        println!("{} {} {kind}", envelope.key, envelope.payload_digest);
    }
    Ok(())
}

fn projection(member: &Member) -> Result<WorkProjection> {
    let reconstruction = member.store.reconstruct(Utc::now())?;
    if !reconstruction.findings.is_empty() {
        return Err(refuse(format!(
            "work payload integrity findings: {:?}",
            reconstruction.findings
        )));
    }
    reconstruction
        .projection
        .ok_or_else(|| refuse("work scope is not declared"))
}

fn inbox(ctx: &Context, args: &InboxArgs) -> Result<()> {
    let member = caller_member(ctx)?;
    let _lock = (!args.peek)
        .then(|| lock_work(ctx, &member.scope.owner))
        .transpose()?;
    let now = Utc::now();
    let projection = projection(&member)?;
    let envelopes = deliverable(&projection, &member.seat, DeliveryAdapter::Pull, now);
    let mut rendered = Vec::new();
    for envelope in envelopes {
        let payload = member.store.read_payload(envelope)?;
        let block = render_for_context(envelope, &payload)?;
        rendered.push((envelope, block));
    }
    if !args.peek {
        for (envelope, _) in &rendered {
            member.store.append_receipt(&Receipt::for_envelope(
                envelope,
                ObserverId::Adapter {
                    adapter: DeliveryAdapter::Pull,
                },
                now,
                Stage::DeliveryAttempted {
                    adapter: DeliveryAdapter::Pull,
                    mechanism: "cs work inbox".to_owned(),
                    outcome: DeliveryOutcome::Submitted,
                },
            ))?;
            member.store.append_receipt(&Receipt::for_envelope(
                envelope,
                ObserverId::Adapter {
                    adapter: DeliveryAdapter::Pull,
                },
                now,
                Stage::ContextDelivered {
                    adapter: DeliveryAdapter::Pull,
                    observation: ContextObservation::Unknown {
                        reason: "tool stdout; model input not observable".to_owned(),
                    },
                },
            ))?;
        }
    }
    if ctx.json {
        let items: Vec<_> = rendered
            .iter()
            .map(|(envelope, block)| serde_json::json!({"envelope": envelope, "context": block}))
            .collect();
        println!("{}", serde_json::to_string(&items)?);
    } else if rendered.is_empty() {
        println!("No deliverable messages.");
    } else {
        for (_, block) in rendered {
            println!("{block}");
        }
    }
    Ok(())
}

fn ack(ctx: &Context, args: &AckArgs) -> Result<()> {
    let member = caller_member(ctx)?;
    let _lock = lock_work(ctx, &member.scope.owner)?;
    if args
        .note
        .as_ref()
        .is_some_and(|note| note.len() as u64 > member.scope.budget.max_payload_bytes)
    {
        return Err(refuse("consumption note exceeds the work payload limit"));
    }
    let key = parse_key(&args.key)?;
    let reply = args.reply.as_deref().map(parse_key).transpose()?;
    let disposition = if args.considered {
        Disposition::Considered
    } else if args.deferred {
        Disposition::Deferred
    } else {
        Disposition::Rejected
    };
    let note_digest = args
        .note
        .as_ref()
        .map(|note| Hash::of_bytes(note.as_bytes()));
    let outcome = accept_consumption(
        &projection(&member)?,
        &member.seat,
        &key,
        disposition,
        reply,
        note_digest,
        Utc::now(),
    )
    .map_err(|error| refuse(error.to_string()))?;
    match outcome {
        Consumption::Record(receipt) => {
            if let (Some(note), Some(digest)) = (&args.note, note_digest) {
                save_note(&member.scope.owner, ctx, &digest, note)?;
            }
            member.store.append_receipt(&receipt)?;
            println!("{key} consumed: {disposition:?}");
        }
        Consumption::Duplicate(_) => println!("{key} already consumed"),
    }
    Ok(())
}

fn save_note(owner: &MoleculeId, ctx: &Context, digest: &Hash, note: &str) -> Result<()> {
    let dir = molecule_dir(ctx, owner).join("work/notes");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join(digest.to_hex());
    if path.exists() {
        if Hash::of_bytes(&fs::read(path)?) != *digest {
            return Err(refuse("existing consumption note differs from its digest"));
        }
    } else {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(note.as_bytes())?;
        file.sync_all()?;
    }
    Ok(())
}

fn list(ctx: &Context, args: &ListArgs) -> Result<()> {
    let owner = match &args.owner {
        Some(owner) => parse_id(owner)?,
        None => caller_member(ctx)?.scope.owner,
    };
    let store = FileWorkMessageStore::new(existing_molecule_dir(ctx, &owner)?);
    let reconstruction = store.reconstruct(Utc::now())?;
    let mut projection = reconstruction
        .projection
        .ok_or_else(|| refuse("work scope is not declared"))?;
    if args.pending {
        projection
            .envelopes
            .retain(|_, view| !view.expired && view.consumed.is_none());
    } else if args.open {
        projection
            .envelopes
            .retain(|_, view| view.consumed.is_none());
    }
    if ctx.json {
        let mut output = serde_json::to_value(&projection)?;
        let findings: Vec<_> = reconstruction
            .findings
            .iter()
            .map(|finding| format!("{finding:?}"))
            .collect();
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "payload_findings".to_owned(),
                serde_json::to_value(findings)?,
            );
        }
        println!("{output}");
    } else {
        for view in projection.envelopes.values() {
            let admitted = view.admitted.map_or("—", |_| "admitted");
            let delivered = match view
                .context_delivered
                .as_ref()
                .map(|stage| &stage.observation)
            {
                Some(ContextObservation::Observed { .. }) => "observed",
                Some(ContextObservation::Unknown { .. }) => "unknown",
                None => "—",
            };
            let consumed = view
                .consumed
                .as_ref()
                .map_or("—", |stage| match stage.disposition {
                    Disposition::Considered => "considered",
                    Disposition::Deferred => "deferred",
                    Disposition::Rejected => "rejected",
                });
            let expired = if view.expired { "expired" } else { "—" };
            println!(
                "{} {} → {}  admitted={admitted}  attempts={}  context={delivered}  consumed={consumed}  expired={expired}",
                view.envelope.key,
                view.envelope.sender,
                view.envelope.recipient,
                view.delivery_attempts.len()
            );
        }
        if projection.envelopes.is_empty() {
            println!("No messages.");
        }
        for finding in reconstruction.findings {
            eprintln!("work integrity finding: {finding:?}");
        }
    }
    Ok(())
}
