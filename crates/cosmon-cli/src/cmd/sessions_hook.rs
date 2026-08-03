// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sessions hook` — the co-pilotage bootstrap (mission co-pilotage M6).
//!
//! M5 gave the mission a cockpit an operator types into. Everything in it
//! works, and nothing in it happens unless somebody remembers to type it. A
//! co-pilot that is only present when its pilot remembers to say so is not a
//! co-pilot; it is a command.
//!
//! This is the part that runs without being typed. Four verbs:
//!
//! - `install` / `uninstall` wire the provider's own hook mechanism, using
//!   [`cosmon_core::copilot_hook`]'s document rules — append beside a
//!   stranger's hook, never replace one, and leave no residue behind;
//! - `run` is what the provider then invokes at each moment;
//! - `status` says whether it is wired, and what it has cost.
//!
//! # What `run` does, and the three things it refuses
//!
//! It pings presence, so the other pilot's `cs sessions peers` shows this one
//! alive. It drains this session's mailbox at a turn boundary and prints the
//! envelopes, so a peer's message arrives while the pilot can still act on it.
//! It publishes a *staged* checkpoint at a natural transition. Then it appends
//! one line to a cost ledger and exits 0.
//!
//! **It never claims a seat.** The ping carries no `--role`, so
//! `cs presence`'s carry-forward keeps whatever the operator set by hand. A
//! hook that pinged `--role primary` would let a process nobody watches take
//! the authority D6 reserves for an operator gesture — and it would do it
//! every thirty seconds, which is the shape of a takeover nobody decided.
//!
//! **It never writes a checkpoint's content.** A checkpoint is the pilot's
//! hypotheses, next moves and open questions; a hook knows none of those, and
//! a hook that invented them would publish a hand-over record whose author
//! never held those positions — and `cs sessions drift` would then compare it
//! as if a mind were behind it. So the content comes from
//! `cs sessions checkpoint stage`, written by the pilot in its own words, and
//! the hook contributes the one thing it does know: *when*. No draft, no
//! publication; the hook says so on stderr and nothing is fabricated.
//!
//! **It never touches the other session.** The channel is the file mailbox and
//! nothing else — no tmux pane, no key, no write into a provider log
//! (OBSERVATION-NEUTRE, ADR-168 §D3.6). What it prints goes to *its own*
//! stdout, which its own provider feeds back to its own model. That is the
//! provider's mechanism for informing the pilot it belongs to, and it stops at
//! this session's boundary.
//!
//! # Exit code is always 0
//!
//! The same rule the briefing-receipt hook lives under, for the same reason: a
//! `UserPromptSubmit` hook that exits non-zero **blocks the prompt**. A
//! co-pilot channel that can stop its pilot from working is worse than no
//! channel. Every path here — unreadable state, missing mailbox, refused
//! ledger — ends in 0, and the failure is reported on stderr where a human
//! reads it without the fleet stopping.

use std::io::Write as _;
use std::path::PathBuf;

use chrono::Utc;
use cosmon_core::copilot_hook::{self, HookEdit, HookEvent, HookProvider, HOOK_TIMEOUT_SECONDS};
use cosmon_core::id::SessionId;
use cosmon_pilot_checkpoint::{CheckpointStore, PilotCheckpoint};

use super::presence;
use super::Context;

pub use cosmon_core::copilot_hook::HOOK_OFF_ENV;

/// `cs sessions hook <sub>`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Sub,
}

/// The hook verbs.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Wire this pilot's provider to run the co-pilotage hook.
    Install(InstallArgs),
    /// Remove the co-pilotage hook, leaving the rest of the file untouched.
    Uninstall(InstallArgs),
    /// Report whether the hook is wired, and what it has cost.
    Status(StatusArgs),
    /// The hook body — invoked by the provider, not usually by a human.
    Run(RunArgs),
}

/// Arguments shared by `install` and `uninstall`.
#[derive(clap::Args, Default)]
pub struct InstallArgs {
    /// The pilot whose configuration to wire: `claude` or `codex`.
    #[arg(long, value_name = "NAME")]
    pub provider: String,
    /// The settings file to edit. Defaults to the provider's own — for Claude
    /// `.claude/settings.local.json` beside the current directory, for Codex
    /// `$CODEX_HOME/config.toml` or `~/.codex/config.toml`.
    #[arg(long, value_name = "PATH")]
    pub settings: Option<PathBuf>,
    /// The `cs` binary the hook should invoke. Defaults to this executable.
    #[arg(long = "cs-bin", value_name = "PATH")]
    pub cs_bin: Option<PathBuf>,
    /// Print what would be written without writing it.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `cs sessions hook status`.
#[derive(clap::Args, Default)]
pub struct StatusArgs {
    /// Restrict the report to one provider.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// The settings file to inspect, when it is not the provider's default.
    #[arg(long, value_name = "PATH")]
    pub settings: Option<PathBuf>,
    /// The session whose cost ledger to summarise. Defaults to
    /// `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
}

/// Arguments for `cs sessions hook run`.
#[derive(clap::Args, Default)]
pub struct RunArgs {
    /// Which moment fired: `session-start`, `turn-start` or `turn-end`.
    #[arg(long, value_name = "EVENT")]
    pub event: String,
    /// The pilot this hook runs inside. Inferred from the payload when it
    /// names one; `claude` otherwise.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// This session's cosmon id. Defaults to `$COSMON_SESSION_ID`.
    #[arg(long, value_name = "SID")]
    pub session: Option<String>,
    /// The provider's payload. Codex passes it as this trailing argument;
    /// Claude pipes it on stdin, which is read when this is absent.
    #[arg(value_name = "PAYLOAD")]
    pub payload: Option<String>,
}

/// Dispatch.
///
/// # Errors
///
/// `install`, `uninstall` and `status` propagate their failures — they are
/// operator gestures and a silent failure would be worse than a message.
/// `run` never returns an error: see the module docs on why its exit code is
/// always 0.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Sub::Install(a) => run_install(ctx, a, Direction::Install),
        Sub::Uninstall(a) => run_install(ctx, a, Direction::Uninstall),
        Sub::Status(a) => run_status(ctx, a),
        Sub::Run(a) => {
            run_hook(ctx, a);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// install / uninstall
// ---------------------------------------------------------------------------

/// Which way the wiring goes. One implementation, because the two verbs differ
/// only in which pure function they call and which verb they print.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Install,
    Uninstall,
}

impl Direction {
    const fn verb(self) -> &'static str {
        match self {
            Self::Install => "installed",
            Self::Uninstall => "removed",
        }
    }
}

/// Where a provider keeps the file this hook is written into.
///
/// Claude's default is the *project-local, user-private* overlay rather than
/// `~/.claude/settings.json`: a co-pilot seat belongs to the repository whose
/// mission is being flown, and `settings.local.json` is the file Claude Code
/// already treats as this developer's own.
fn default_settings(provider: HookProvider) -> anyhow::Result<PathBuf> {
    match provider {
        HookProvider::Claude => Ok(PathBuf::from(".claude").join("settings.local.json")),
        HookProvider::Codex => {
            let home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot resolve a Codex home — pass --settings with the path to config.toml"
                    )
                })?;
            Ok(home.join("config.toml"))
        }
    }
}

fn resolve_cs_bin(args: &InstallArgs) -> String {
    args.cs_bin
        .clone()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("cs"))
        .display()
        .to_string()
}

fn run_install(ctx: &Context, args: &InstallArgs, direction: Direction) -> anyhow::Result<()> {
    let provider = HookProvider::parse(&args.provider).map_err(|e| anyhow::anyhow!(e))?;
    let path = match &args.settings {
        Some(p) => p.clone(),
        None => default_settings(provider)?,
    };
    let shown = path.display().to_string();
    // A missing file is not an error: it is the ordinary case on a machine
    // where nobody has written settings yet.
    let existing = std::fs::read_to_string(&path).ok();
    let cs_bin = resolve_cs_bin(args);

    let edit: HookEdit = match (provider, direction) {
        (HookProvider::Claude, Direction::Install) => {
            copilot_hook::install_claude(&shown, existing.as_deref(), &cs_bin)?
        }
        (HookProvider::Claude, Direction::Uninstall) => {
            copilot_hook::uninstall_claude(&shown, existing.as_deref())?
        }
        (HookProvider::Codex, Direction::Install) => {
            copilot_hook::install_codex(&shown, existing.as_deref(), &cs_bin)?
        }
        (HookProvider::Codex, Direction::Uninstall) => {
            copilot_hook::uninstall_codex(&shown, existing.as_deref())?
        }
    };

    if edit.changed && !args.dry_run {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&path, &edit.document)?;
    }

    let events: Vec<&str> = edit.events.iter().map(|e| e.as_str()).collect();
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "provider": provider.as_str(),
                "settings": shown,
                "changed": edit.changed,
                "dry_run": args.dry_run,
                "events": events,
                "timeout_seconds": HOOK_TIMEOUT_SECONDS,
            })
        );
        if args.dry_run {
            print!("{}", edit.document);
        }
        return Ok(());
    }

    if args.dry_run {
        println!("--- {shown} (dry run, nothing written) ---");
        print!("{}", edit.document);
        return Ok(());
    }
    if edit.changed {
        println!(
            "{verb} the co-pilotage hook in {shown} [{events}]",
            verb = direction.verb(),
            events = events.join(", "),
        );
    } else {
        println!("{shown} already says what it should — nothing written");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// status — is it wired, and what has it cost
// ---------------------------------------------------------------------------

fn run_status(ctx: &Context, args: &StatusArgs) -> anyhow::Result<()> {
    let providers: Vec<HookProvider> = match &args.provider {
        Some(raw) => vec![HookProvider::parse(raw).map_err(|e| anyhow::anyhow!(e))?],
        None => vec![HookProvider::Claude, HookProvider::Codex],
    };
    if args.settings.is_some() && providers.len() != 1 {
        return Err(anyhow::anyhow!(
            "--settings names one file, so it needs --provider to say whose it is"
        ));
    }

    let sid = presence::resolve_or_derive_sid(args.session.as_deref())?;
    let cost = read_cost(ctx, &sid);
    let off = hook_is_off();

    let mut rows = Vec::new();
    for provider in providers {
        let path = match &args.settings {
            Some(p) => p.clone(),
            None => default_settings(provider)?,
        };
        let shown = path.display().to_string();
        let existing = std::fs::read_to_string(&path).ok();
        let wired = match provider {
            HookProvider::Claude => copilot_hook::installed_claude(&shown, existing.as_deref())?,
            HookProvider::Codex => copilot_hook::installed_codex(&shown, existing.as_deref())?,
        };
        rows.push((provider, shown, wired));
    }

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "session": sid.as_str(),
                "disabled": off,
                "providers": rows.iter().map(|(p, path, wired)| serde_json::json!({
                    "provider": p.as_str(),
                    "settings": path,
                    "installed": !wired.is_empty(),
                    "events": wired.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "cost": cost.as_json(),
            })
        );
        return Ok(());
    }

    for (provider, path, wired) in &rows {
        let state = if wired.is_empty() {
            "not installed".to_owned()
        } else {
            wired
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("{:<8}  {:<12}  {path}", provider.as_str(), state);
    }
    if off {
        println!("(disabled: ${HOOK_OFF_ENV} is set — the hook returns before it reads anything)");
    }
    println!("{}", cost.render(sid.as_str()));
    Ok(())
}

// ---------------------------------------------------------------------------
// the cost ledger — the acceptance clause that needs a number
// ---------------------------------------------------------------------------

/// One line of the ledger: what a single hook invocation did and what it cost.
///
/// The mission asks for cost *measured*, not estimated, so this records what
/// actually happened — wall-clock and the bytes the hook put into the pilot's
/// context — rather than a model of it. Both are what a pilot pays: latency at
/// a turn boundary, and context that is no longer available for the mission.
#[derive(serde::Serialize, serde::Deserialize)]
struct HookCost {
    at: chrono::DateTime<Utc>,
    event: String,
    provider: String,
    duration_ms: u64,
    /// Bytes printed to a stdout the provider feeds back to the model. Zero at
    /// a moment whose stdout is discarded — the honest number, not the number
    /// of bytes written somewhere.
    injected_bytes: usize,
    messages: usize,
    checkpoint_published: bool,
}

/// What the ledger says, summarised.
#[derive(Default)]
struct CostSummary {
    runs: usize,
    total_ms: u64,
    max_ms: u64,
    injected_bytes: usize,
    messages: usize,
    checkpoints: usize,
}

impl CostSummary {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "runs": self.runs,
            "total_ms": self.total_ms,
            "max_ms": self.max_ms,
            "mean_ms": self.mean_ms(),
            "injected_bytes": self.injected_bytes,
            "messages": self.messages,
            "checkpoints": self.checkpoints,
        })
    }

    fn mean_ms(&self) -> u64 {
        if self.runs == 0 {
            0
        } else {
            self.total_ms / self.runs as u64
        }
    }

    fn render(&self, sid: &str) -> String {
        if self.runs == 0 {
            return format!("cost: the hook has not run yet for {sid}");
        }
        format!(
            "cost: {runs} runs, {mean} ms mean / {max} ms worst, {bytes} B injected, \
             {messages} messages, {checkpoints} checkpoints",
            runs = self.runs,
            mean = self.mean_ms(),
            max = self.max_ms,
            bytes = self.injected_bytes,
            messages = self.messages,
            checkpoints = self.checkpoints,
        )
    }
}

fn cost_ledger(ctx: &Context, sid: &SessionId) -> PathBuf {
    presence::state_root(ctx)
        .join("pilot-hooks")
        .join(format!("{}.cost.jsonl", sanitize(sid.as_str())))
}

/// Make a session id safe as one path component.
///
/// A session id is a free string (ADR-168 §D2) and a free string can hold a
/// `/` or a `..`. The ledger is a file named after it, so the id is mapped to
/// a single component here rather than trusted — the same rule the receipt
/// nonce lives under.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn read_cost(ctx: &Context, sid: &SessionId) -> CostSummary {
    let mut summary = CostSummary::default();
    let Ok(text) = std::fs::read_to_string(cost_ledger(ctx, sid)) else {
        return summary;
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<HookCost>(line) else {
            continue;
        };
        summary.runs += 1;
        summary.total_ms += row.duration_ms;
        summary.max_ms = summary.max_ms.max(row.duration_ms);
        summary.injected_bytes += row.injected_bytes;
        summary.messages += row.messages;
        summary.checkpoints += usize::from(row.checkpoint_published);
    }
    summary
}

fn append_cost(ctx: &Context, sid: &SessionId, row: &HookCost) -> std::io::Result<()> {
    let path = cost_ledger(ctx, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(row).unwrap_or_default();
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())
}

// ---------------------------------------------------------------------------
// staged checkpoints — the pilot writes the content, the hook picks the moment
// ---------------------------------------------------------------------------

/// Where `cs sessions checkpoint stage` leaves a draft for the hook to publish.
///
/// One file per session, overwritten by each stage: a draft is what this pilot
/// would hand over *now*, and a queue of stale drafts published later would be
/// a hand-over record of a mind that has moved on.
pub(crate) fn draft_path(ctx: &Context, sid: &SessionId) -> PathBuf {
    presence::state_root(ctx)
        .join("pilot-hooks")
        .join(format!("{}.draft.json", sanitize(sid.as_str())))
}

/// Write a staged checkpoint. Called by `cs sessions checkpoint stage`.
///
/// # Errors
///
/// Propagates filesystem and serialisation failures — staging is an operator
/// gesture, and one that silently did not land is worse than one that said so.
pub(crate) fn stage(ctx: &Context, cp: &PilotCheckpoint) -> anyhow::Result<PathBuf> {
    let path = draft_path(ctx, &SessionId::new(cp.session_id.as_str().to_owned())?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cp)?)?;
    Ok(path)
}

/// Publish the staged draft, if there is one, and clear it.
///
/// Returns `Ok(None)` when nothing was staged, which is the ordinary case and
/// not a failure: a pilot that has not written a hand-over record has not
/// written one, and the hook does not write it for them.
fn publish_staged(ctx: &Context, sid: &SessionId) -> anyhow::Result<Option<PilotCheckpoint>> {
    let path = draft_path(ctx, sid);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let cp: PilotCheckpoint = serde_json::from_str(&raw)?;
    CheckpointStore::new(presence::state_root(ctx)).publish(&cp)?;
    // Removed only after the publication landed: a crash between the two costs
    // a duplicate publication of an identical record, which is idempotent,
    // rather than a hand-over record that exists nowhere.
    std::fs::remove_file(&path).ok();
    Ok(Some(cp))
}

// ---------------------------------------------------------------------------
// run — the hook body
// ---------------------------------------------------------------------------

/// Is the hook switched off for this process?
fn hook_is_off() -> bool {
    std::env::var_os(HOOK_OFF_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Read the provider's payload: the trailing argument if there is one, stdin
/// otherwise.
///
/// stdin is drained to end whether or not it is needed, so a provider writing
/// into the hook's pipe never sees it close early.
fn read_payload(args: &RunArgs) -> String {
    use std::io::Read as _;
    if let Some(p) = &args.payload {
        return p.clone();
    }
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    buf
}

/// What the payload tells us about the session it came from.
///
/// Only two fields are read, and neither is conversation content: the native
/// session id, which is half the canonical selector, and nothing else. This is
/// the confidentiality ceiling ADR-168 set for the whole mission.
fn native_session_id(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    v.get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// The hook body. Never fails outward — see the module docs.
fn run_hook(ctx: &Context, args: &RunArgs) {
    if hook_is_off() {
        return;
    }
    let started = std::time::Instant::now();

    let Ok(event) = HookEvent::parse(&args.event) else {
        eprintln!(
            "cs sessions hook: unknown event {:?} — doing nothing",
            args.event
        );
        return;
    };
    let provider = match args.provider.as_deref() {
        Some(raw) => match HookProvider::parse(raw) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("cs sessions hook: {e} — doing nothing");
                return;
            }
        },
        None => HookProvider::Claude,
    };
    let payload = read_payload(args);

    let Ok(sid) = presence::resolve_or_derive_sid(args.session.as_deref()) else {
        eprintln!(
            "cs sessions hook: no session id ($COSMON_SESSION_ID unset and no tty) — \
             nothing to be present as"
        );
        return;
    };

    // 1. Presence. No role, no follows, no capabilities: every co-pilotage
    //    field is carried forward from the snapshot the operator wrote. This
    //    heartbeat says "still here", never "I am in command".
    if let Err(e) = presence::ping(
        ctx,
        &presence::PingArgs {
            session: Some(sid.as_str().to_owned()),
            provider: Some(provider.as_str().to_owned()),
            native_session_id: native_session_id(&payload),
            galaxy: "cosmon".to_owned(),
            ..presence::PingArgs::default()
        },
    ) {
        eprintln!("cs sessions hook: presence ping failed: {e}");
    }

    // 2. The mailbox, but only where the pilot can read what comes out.
    let mut injected = 0usize;
    let mut messages = 0usize;
    if event.stdout_reaches_pilot(provider) {
        match presence::collect_inbox(
            ctx,
            &presence::InboxArgs {
                session: Some(sid.as_str().to_owned()),
                peek: false,
                all: false,
            },
        ) {
            Ok((sid_read, rendered)) if !rendered.is_empty() => {
                let block = render_inbox(&rendered);
                print!("{block}");
                injected = block.len();
                messages = rendered.len();
                // Acknowledge only after the text has left this process, which
                // is the same ordering `cs sessions inbox` uses: a crash now
                // costs a re-read, never a lost message (MESSAGE-TRACE).
                std::io::stdout().flush().ok();
                if let Err(e) = presence::ack_consumed(ctx, &sid_read, &rendered) {
                    eprintln!("cs sessions hook: could not acknowledge: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("cs sessions hook: mailbox unreadable: {e}"),
        }
    }

    // 3. A staged checkpoint, at a transition.
    let mut published = false;
    if matches!(event, HookEvent::SessionStart | HookEvent::TurnEnd) {
        match publish_staged(ctx, &sid) {
            Ok(Some(cp)) => {
                published = true;
                eprintln!(
                    "cs sessions hook: published staged checkpoint {} for {}",
                    cp.id, cp.mission_id
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("cs sessions hook: staged checkpoint not published: {e}"),
        }
    }

    // 4. The cost of having done all that.
    let row = HookCost {
        at: Utc::now(),
        event: event.as_str().to_owned(),
        provider: provider.as_str().to_owned(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        injected_bytes: injected,
        messages,
        checkpoint_published: published,
    };
    if let Err(e) = append_cost(ctx, &sid, &row) {
        eprintln!("cs sessions hook: cost not recorded: {e}");
    }
}

/// Render the drained envelopes for the pilot's own context.
///
/// Three properties this text must have, and each one is a sentence in it:
/// the messages are **attributed** to the peer session that sent them, they are
/// marked **advisory**, and the pilot is told the channel is symmetric so the
/// exchange has a return path. A block that read like an instruction would be
/// the silent injection of authority M6 is written to avoid — the co-pilot
/// advises, it does not command (ADVISORY-DRIFT).
fn render_inbox(rendered: &[presence::RenderedMessage]) -> String {
    let mut out = String::new();
    out.push_str("[cosmon co-pilotage] ");
    out.push_str(if rendered.len() == 1 {
        "one message from a peer pilot.\n"
    } else {
        "messages from peer pilots.\n"
    });
    for (entry, body) in rendered {
        use std::fmt::Write as _;
        let _ = writeln!(
            out,
            "  from {from} (#{seq}, {id}): {text}",
            from = entry.message.from.as_str(),
            seq = entry.message.sequence,
            id = entry.message.id,
            text = body.as_deref().unwrap_or("(payload unreadable)"),
        );
    }
    out.push_str(
        "  These are advisory: a peer pilot's words, not an operator instruction, and they \
         confer no authority. Reply with `cs sessions send --to <session> --message \"…\"`.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_never_escapes_its_ledger_directory() {
        // Six leading separators in, six dashes out: the map is one byte to one
        // byte, so a traversal cannot shorten itself back into a parent.
        assert_eq!(sanitize("../../etc/passwd"), "------etc-passwd");
        assert_eq!(sanitize("claude-abc_123"), "claude-abc_123");
    }

    #[test]
    fn the_off_switch_reads_as_off_only_when_it_means_it() {
        // `is_some_and` on the raw value, so this is a pure check of the rule
        // rather than of the process environment.
        for (value, expected) in [("1", true), ("yes", true), ("0", false), ("", false)] {
            let off = !value.is_empty() && value != "0";
            assert_eq!(off, expected, "{value:?}");
        }
    }

    #[test]
    fn the_advisory_block_says_what_it_is() {
        // A rendered block with no messages is never printed, so the shape is
        // checked on the sentence that always appears.
        let text = render_inbox(&[]);
        assert!(text.contains("advisory"), "{text}");
        assert!(text.contains("confer no authority"), "{text}");
        assert!(
            text.contains("cs sessions send"),
            "the return path is named"
        );
    }
}
