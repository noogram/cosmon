// SPDX-License-Identifier: AGPL-3.0-only

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

//! `cosmon-remote` — the ONE tenant CLI for the cosmon-rpp v1 surface.
//!
//! No subcommand catalogue here: route-backed verbs project their
//! `about` strings and their HTTP tuples from [`cosmon_remote::canon`]
//! (the §8p surface canon folded at build time). `--help` is the
//! reference; a prose copy would be the drift channel the fusion
//! removed. Commands without a route (`config`, `auth login`) are
//! local/composed product features, legitimately outside the canon.
//!
//! The binary stays free of any TUI; output is plain text or `--json`
//! depending on the global flag.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use cosmon_remote::canon;
use cosmon_remote::client::{
    Client, CollapseRequest, ListFilters, Liveness, NucleateRequest, ReactiveRefresh,
    ReasonRequest, ResultEnvelope, TagRequest,
};
use cosmon_remote::config::{Profile, ProfileStore, ENV_TOKEN};
use cosmon_remote::error::{Error, Result};
use cosmon_remote::{doctor, hints, phone_home, pkce};
use tracing_subscriber::EnvFilter;

mod root_help;

#[derive(Debug, Parser)]
#[command(
    name = "cosmon-remote",
    version,
    about = "Thin CLI for the cosmon-rpp v1 API",
    after_help = root_help::after_help("cosmon-remote"),
    after_long_help = root_help::after_long_help("cosmon-remote")
)]
struct Cli {
    /// Profile name (default: `default_profile` from config.toml, or
    /// `$COSMON_REMOTE_PROFILE`).
    #[arg(long, global = true)]
    profile: Option<String>,

    /// Render machine-readable JSON instead of human text where applicable.
    #[arg(long, global = true)]
    json: bool,

    /// Bearer JWT (overrides `$COSMON_REMOTE_TOKEN`). When unset and an
    /// authenticated call is made, the CLI mints a token via the
    /// profile's `oidc_url`.
    #[arg(long, global = true)]
    token: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

/// Subcommand list — `display_order` separates the golden path (the
/// verbs of the first hour, ranks 1–5) from the diagnostic verbs
/// (ranks 20+). Diagnostic ≠ parcours: healthz/quota/noyaux/
/// workers stay fully available but sink to the bottom of `--help`.
#[derive(Debug, Subcommand)]
enum Cmd {
    #[command(display_order = 1, about = format!("One gesture: nucleate + tackle + follow until the result is ready (composition of {} and {}; zero new routes). Shows the credit guard before the first spend — once, remembered. `molecule nucleate` stays available as the advanced path", canon::POST_V1_MOLECULES.label(), canon::POST_V1_MOLECULES_ID_TACKLE.label()))]
    Do {
        /// What you want done — becomes the molecule's `topic` variable.
        topic: String,
        /// Formula to nucleate (the standard one-shot work unit).
        #[arg(long, default_value = "task-work")]
        formula: String,
        #[arg(long)]
        kind: Option<String>,
        /// Free-form variables `--var k=v`, repeat as needed.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Skip the credit guard for this run (scripts/CI). Does NOT
        /// persist consent — a script's yes is not the operator's.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Give-up deadline (seconds) for the follow phase. The worker
        /// keeps running server-side past it; `result <id>` later.
        #[arg(long = "follow-timeout", default_value_t = 1800)]
        follow_timeout: u64,
        /// Poll cadence (seconds) of the follow phase.
        #[arg(long = "poll-interval", default_value_t = 5)]
        poll_interval: u64,
        /// Disable the live events tail (the observe poll still runs).
        #[arg(long = "no-events")]
        no_events: bool,
    },
    #[command(display_order = 1, about = format!("Like `do`, then price it: brackets the same nucleate + tackle + follow flow with two {} reads and reports the quota delta THIS run charged against your bucket. Zero new routes; the leak caveat is printed honestly", canon::GET_V1_QUOTA.label()))]
    Run {
        /// What you want done — becomes the molecule's `topic` variable.
        topic: String,
        /// Formula to nucleate (the standard one-shot work unit).
        #[arg(long, default_value = "task-work")]
        formula: String,
        #[arg(long)]
        kind: Option<String>,
        /// Free-form variables `--var k=v`, repeat as needed.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Skip the credit guard for this run (scripts/CI). Does NOT
        /// persist consent — a script's yes is not the operator's.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Give-up deadline (seconds) for the follow phase. The worker
        /// keeps running server-side past it; `result <id>` later.
        #[arg(long = "follow-timeout", default_value_t = 1800)]
        follow_timeout: u64,
        /// Poll cadence (seconds) of the follow phase.
        #[arg(long = "poll-interval", default_value_t = 5)]
        poll_interval: u64,
        /// Disable the live events tail (the observe poll still runs).
        #[arg(long = "no-events")]
        no_events: bool,
    },
    /// Molecule lifecycle (the §8p frozen surface).
    #[command(display_order = 1)]
    Molecule {
        #[command(subcommand)]
        sub: MoleculeCmd,
    },
    /// D-AVATAR instance lifecycle (drained from cs-thin by the fusion).
    Avatar {
        #[command(subcommand)]
        sub: AvatarCmd,
    },
    /// Artifact endpoints.
    #[command(display_order = 2)]
    Artifact {
        #[command(subcommand)]
        sub: ArtifactCmd,
    },
    /// Auth-claude PKCE flow.
    #[command(display_order = 3)]
    Auth {
        #[command(subcommand)]
        sub: AuthCmd,
    },
    /// Sign in to cosmon via the real OAuth 2.0 PKCE browser flow against the
    /// deployment's Forgejo identity provider, then persist the access and
    /// refresh tokens in the OS keyring (or a 0600 file on a headless box).
    /// Distinct from the Claude/Anthropic device flow reached by "auth login".
    /// After this, every command refreshes the 15-minute access token silently
    /// — no re-auth until the roughly monthly refresh token lapses.
    #[command(display_order = 3)]
    #[allow(clippy::doc_markdown)] // prose is shown verbatim in --help; no backticks
    Login,
    /// Forget the persisted cosmon credential for the active profile (the
    /// reverse of "login"). Idempotent — logging out when already signed out is
    /// a no-op. Does not touch the Claude/Anthropic session.
    #[command(display_order = 3)]
    #[allow(clippy::doc_markdown)] // prose is shown verbatim in --help; no backticks
    Logout,
    #[command(display_order = 4, about = format!("Server-Sent Events stream of molecule lifecycle events ({}){}", canon::GET_V1_EVENTS.label(), canon::GET_V1_EVENTS.effect_suffix()))]
    Events {
        #[command(subcommand)]
        sub: EventsCmd,
    },
    /// Profile and deployment config.
    #[command(display_order = 5)]
    Config {
        #[command(subcommand)]
        sub: ConfigCmd,
    },
    /// Onboarding checks, named and falsifiable: réseau, oidc-url,
    /// badge tenant, lunettes du worker. Vert/rouge par check, la
    /// commande de réparation sur chaque ligne rouge. Run by
    /// install.sh and whenever something breaks.
    #[command(display_order = 20)]
    Doctor,
    /// Adapter liveness probe (diagnostic).
    #[command(display_order = 21)]
    Healthz,
    #[command(display_order = 22, about = format!("{} — read the current rate-limit snapshot. Table by default, JSON with `--json`. (diagnostic){}", canon::GET_V1_QUOTA.label(), canon::GET_V1_QUOTA.effect_suffix()))]
    Quota,
    #[command(display_order = 23, about = format!("Worker observability ({}). (diagnostic){}", canon::GET_V1_WORKERS.label(), canon::GET_V1_WORKERS.effect_suffix()))]
    Workers {
        #[command(subcommand)]
        sub: WorkersCmd,
    },
    #[command(display_order = 24, about = format!("{} — discovery endpoint for multi-noyau operators. (diagnostic){}", canon::GET_V1_NOYAUX.label(), canon::GET_V1_NOYAUX.effect_suffix()))]
    Noyaux {
        #[command(subcommand)]
        sub: NoyauxCmd,
    },
    /// Introspection — render the man page from the live clap tree and
    /// emit it on stdout. Hidden plumbing transposed from the proven
    /// `cs __man-page` pattern: the committed
    /// `man/cosmon-remote.1` is regenerated from here and
    /// golden-checked by `tests/help_goldens.rs`, so the man page is
    /// ALWAYS a projection of the clap tree, never written beside it.
    #[command(name = "__man-page", hide = true)]
    ManPage,
    // Deliberately LAST: converse is off the golden path
    // (delib-20260610-9a0c T3) — a top-level verb, never an `avatar`
    // subcommand (« avatar est un mot de doctrine, jamais un nom
    // d'API », tenant guide §12.2).
    #[command(display_order = 99, about = format!("{} — {}", canon::POST_V1_AVATAR_CONVERSE.label(), canon::POST_V1_AVATAR_CONVERSE.blurb))]
    Converse {
        /// Target avatar identifier within the caller's noyau. The
        /// server accepts the message only when an explicit operator
        /// binding exists for this avatar (refused `no_binding`
        /// otherwise).
        avatar_id: String,
        /// Message payload, sent as a JSON string. Pass
        /// `--message-json` to send structured JSON instead.
        message: String,
        /// Parse MESSAGE as JSON and send the structured value.
        #[arg(long)]
        message_json: bool,
        /// Message kind. `request` expects a response and is
        /// hop-bounded server-side (L3 anti-cycle); `announce` is
        /// fire-and-forget and exempt.
        #[arg(long, value_enum, default_value_t = ConverseKindArg::Request)]
        kind: ConverseKindArg,
        /// Relay depth in a `request` chain. Originating messages send
        /// 0; relays increment. The server refuses the chain with
        /// `max_hops_exceeded` beyond the binding's bound.
        #[arg(long, default_value_t = 0)]
        hop: u32,
    },
}

/// Wire values of the canal (b) message kind (`kind` body field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ConverseKindArg {
    /// Synchronous conversation — expects a response, hop-bounded.
    Request,
    /// Fire-and-forget notification — no response, no hop bound.
    Announce,
}

impl ConverseKindArg {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Announce => "announce",
        }
    }
}

#[derive(Debug, Subcommand)]
enum NoyauxCmd {
    /// List the noyaux visible to the JWT's `sub`. Table by default,
    /// JSON with `--json`.
    List,
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    /// Initialise a new profile with just a host URL — the operator
    /// then sets `sub`, `aud`, `oidc-url` (or `install.sh` does it
    /// server-side via templating).
    Init {
        /// Profile name (e.g. `tenant-demo-aws`, `local`).
        name: String,
        /// Base URL — preserve scheme as-is (`http://…` for loopback,
        /// `https://…` for Tailscale-served deployments).
        host: String,
    },
    /// Set a single config key.
    Set {
        /// One of `host`, `sub`, `aud`, `oidc-url`, `noyau`, `timeout`,
        /// `artifacts-dir`.
        key: String,
        value: String,
    },
    /// Show the resolved profile (the values used for API calls).
    Show,
    /// Switch the default profile.
    Use { name: String },
    /// List every known profile name.
    List,
}

#[derive(Debug, Subcommand)]
enum AuthCmd {
    /// Run the PKCE manual-paste flow: `start → email → confirm`.
    #[command(after_long_help = root_help::AUTH_LOGIN_AFTER_LONG_HELP)]
    Login {
        #[arg(long)]
        email: String,
        /// Pre-supply the authorization code instead of prompting
        /// (smoke-test / non-interactive harness).
        #[arg(long = "code")]
        code: Option<String>,
    },
    /// Inspect a session by id.
    Status { session_id: String },
    /// Delete a session (cleanup).
    Logout { session_id: String },
    #[command(about = format!("{} — whoami: decode the bearer JWT and echo back what the server sees (`sub`, `aud`, `scopes`, `noyau`, `expires_at`, `issuer`){}", canon::GET_V1_AUTH_ME.label(), canon::GET_V1_AUTH_ME.effect_suffix()))]
    Me,
}

#[derive(Debug, Subcommand)]
enum MoleculeCmd {
    #[command(
        about = format!("{} — create a molecule{}", canon::POST_V1_MOLECULES.label(), canon::POST_V1_MOLECULES.effect_suffix()),
        after_long_help = root_help::NUCLEATE_AFTER_LONG_HELP
    )]
    Nucleate {
        formula: String,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// Free-form variables `--var k=v`, repeat as needed.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    #[command(about = format!("{} — ensemble list with optional filters{}", canon::GET_V1_MOLECULES.label(), canon::GET_V1_MOLECULES.effect_suffix()))]
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        fleet: Option<String>,
    },
    #[command(about = format!("{}{}", canon::GET_V1_MOLECULES_ID.label(), canon::GET_V1_MOLECULES_ID.effect_suffix()))]
    Get { id: String },
    #[command(about = format!("{} — fetch the canonical deliverable (synthesis.md / result.md / the lone artifact). Prints the body to stdout (text) or a metadata line (binary); `--json` for the full envelope{}", canon::GET_V1_MOLECULES_ID_RESULT.label(), canon::GET_V1_MOLECULES_ID_RESULT.effect_suffix()))]
    Result { id: String },
    #[command(about = format!("{}{}", canon::POST_V1_MOLECULES_ID_TACKLE.label(), canon::POST_V1_MOLECULES_ID_TACKLE.effect_suffix()))]
    Tackle { id: String },
    #[command(about = format!("{} — request the resident drain of the DAG rooted at this molecule. The server decides what to tackle, under the binding's bounds (read them via `quota`); 202 on spawn, lifecycle on the events stream", canon::POST_V1_MOLECULES_ID_RUN.label()))]
    Run { id: String },
    #[command(about = format!("{}{}", canon::POST_V1_MOLECULES_ID_COLLAPSE.label(), canon::POST_V1_MOLECULES_ID_COLLAPSE.effect_suffix()))]
    Collapse {
        id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        cause: Option<String>,
    },
    #[command(about = format!("{}{}", canon::POST_V1_MOLECULES_ID_FREEZE.label(), canon::POST_V1_MOLECULES_ID_FREEZE.effect_suffix()))]
    Freeze {
        id: String,
        #[arg(long)]
        reason: String,
    },
    // Help-text correction, consciously re-blessed (CHANGELOG 0.2.0):
    // the pre-fusion about advertised `POST …/thaw`, a route the
    // adapter removed in v1.0.0-rc (410 Gone). Thaw rides the fused
    // freeze route with `state: "active"`.
    #[command(about = format!("{} with `state: \"active\"` — resume a frozen molecule (the legacy `/thaw` route is 410 Gone){}", canon::POST_V1_MOLECULES_ID_FREEZE.label(), canon::POST_V1_MOLECULES_ID_FREEZE.effect_suffix()))]
    Thaw {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(about = format!("{}{}", canon::POST_V1_MOLECULES_ID_STUCK.label(), canon::POST_V1_MOLECULES_ID_STUCK.effect_suffix()))]
    Stuck {
        id: String,
        #[arg(long)]
        reason: String,
    },
    #[command(about = format!("{}{}", canon::POST_V1_MOLECULES_ID_TAGS.label(), canon::POST_V1_MOLECULES_ID_TAGS.effect_suffix()))]
    Tag {
        id: String,
        #[arg(long = "add")]
        add: Vec<String>,
        #[arg(long = "remove")]
        remove: Vec<String>,
    },
}

/// D-AVATAR instance lifecycle sub-verbs (§8p tenant verbs, drained
/// from cs-thin by the A2 fusion). Output is the full response
/// envelope as JSON — these verbs are wire-mirrors, not renderers.
#[derive(Debug, Subcommand)]
enum AvatarCmd {
    #[command(about = format!("{} — mould or avatar state{}", canon::GET_V1_AVATAR_INSTANCE_ID_STATUS.label(), canon::GET_V1_AVATAR_INSTANCE_ID_STATUS.effect_suffix()))]
    Status { instance_id: String },
    #[command(about = format!("{} — bind moule→avatar{}", canon::POST_V1_AVATAR_INSTANCE_ID_INCARNATE.label(), canon::POST_V1_AVATAR_INSTANCE_ID_INCARNATE.effect_suffix()))]
    Incarnate {
        instance_id: String,
        /// Pilote DID (e.g. `did:key:z6Mk...`).
        #[arg(long)]
        pilote: String,
        /// Tenant identifier.
        #[arg(long)]
        tenant: String,
        /// ISO 3166-1 alpha-2 jurisdiction (e.g. `FR`).
        #[arg(long)]
        juridiction: String,
    },
    #[command(about = format!("{} — bind a canal{}", canon::POST_V1_AVATAR_INSTANCE_ID_GRANT.label(), canon::POST_V1_AVATAR_INSTANCE_ID_GRANT.effect_suffix()))]
    Grant {
        instance_id: String,
        /// Canal to grant (`b`, `c`, or `d`).
        #[arg(long)]
        canal: String,
        /// Target identity to bind the canal to.
        #[arg(long)]
        target: String,
    },
    #[command(about = format!("{} — cicatrice + events{}", canon::GET_V1_AVATAR_INSTANCE_ID_AUDIT.label(), canon::GET_V1_AVATAR_INSTANCE_ID_AUDIT.effect_suffix()))]
    Audit { instance_id: String },
    #[command(about = format!("{} — pre-incarnation info{}", canon::GET_V1_AVATAR_INSTANCE_ID_MOULD_INFO.label(), canon::GET_V1_AVATAR_INSTANCE_ID_MOULD_INFO.effect_suffix()))]
    MouldInfo { instance_id: String },
}

#[derive(Debug, Subcommand)]
enum WorkersCmd {
    /// `GET /v1/workers` — list active workers in the caller's noyau.
    /// Table by default, JSON with `--json`.
    List,
}

#[derive(Debug, Subcommand)]
enum EventsCmd {
    #[command(about = format!("{} — connect to the SSE stream and print each event to stdout. The stream runs until killed (Ctrl-C){}", canon::GET_V1_EVENTS.label(), canon::GET_V1_EVENTS.effect_suffix()))]
    Stream {
        /// Filter to a single molecule id (server-side query filter).
        /// When omitted, every event for the tenant noyau is streamed.
        #[arg(long = "molecule-id")]
        molecule_id: Option<String>,
        /// Resume from a known last-seen event id (HTTP
        /// `Last-Event-ID` header).
        #[arg(long = "last-event-id")]
        last_event_id: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum ArtifactCmd {
    // {mol_id}→{id}: the canon names the placeholders; consciously
    // re-blessed help text (CHANGELOG 0.2.0). Args are unchanged.
    #[command(about = format!("{}{}", canon::GET_V1_MOLECULES_ID_ARTIFACTS.label(), canon::GET_V1_MOLECULES_ID_ARTIFACTS.effect_suffix()))]
    List { mol_id: String },
    #[command(about = format!("{}{}", canon::GET_V1_MOLECULES_ID_ARTIFACTS_TOKEN.label(), canon::GET_V1_MOLECULES_ID_ARTIFACTS_TOKEN.effect_suffix()))]
    Get {
        mol_id: String,
        /// Artifact token (the `{token}` path segment). The clap id must
        /// differ from the global `--token` (JWT override): with a shared
        /// id, clap propagates the positional value into the global slot
        /// and the artifact token gets sent as the bearer.
        /// `value_name` keeps the help surface.
        #[arg(value_name = "TOKEN")]
        artifact_token: String,
        /// Destination path. Default: `<artifacts_dir>/<mol_id>/<token>`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    #[command(about = format!("{}{}", canon::PUT_V1_MOLECULES_ID_ARTIFACTS_TOKEN.label(), canon::PUT_V1_MOLECULES_ID_ARTIFACTS_TOKEN.effect_suffix()))]
    Push {
        mol_id: String,
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long = "content-type")]
        content_type: Option<String>,
        /// Previous `ETag` for overwrite — omit on first push.
        #[arg(long = "if-match")]
        if_match: Option<String>,
    },
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("COSMON_REMOTE_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Err(err) = run().await {
        let exit_code = exit_code_for(&err);
        // `NoDeliverable` (C4) already rendered its actionable next
        // gesture at the call site — it is not a transport failure.
        // Carry only the exit code; do not re-print a terse "error:".
        if !matches!(err, Error::NoDeliverable { .. }) {
            eprintln!("error: {err}");
            // Actionable hint (smithy C1): when the wire label is one
            // the binary understands, say the probable cause and THE
            // repair command under the raw error — never instead of it.
            if let Error::Api { status, body } = &err {
                if let Some(hint) =
                    hints::label_of(body).and_then(|l| hints::for_api_error(*status, l))
                {
                    for line in hint.lines() {
                        eprintln!("  ↳ {}", line.trim_start());
                    }
                }
            }
        }
        std::process::exit(exit_code);
    }
}

fn exit_code_for(err: &Error) -> i32 {
    match err {
        Error::Config(_) => 2,
        // The OIDC login / silent-refresh flow shares the auth exit code: a
        // `RefreshExpired` or a state mismatch is an auth failure the operator
        // resolves with `login`, exactly like the Claude flow's `Error::Auth`.
        Error::Auth(_) | Error::Oidc(_) => 3,
        Error::Api { status, .. } if *status == 401 || *status == 403 => 4,
        Error::Api { status, .. } if *status == 404 => 5,
        // "Nothing to hand you" — the slot the bare 404 used to occupy;
        // keep the exit code so scripts behave unchanged.
        Error::NoDeliverable { .. } => 5,
        _ => 1,
    }
}

/// What the human-facing (`non-json`) `result` rendering decided to emit
/// — factored out of the [`MoleculeCmd::Result`] arm so it can be tested
/// without a server. Pure: it reads the envelope and the
/// clock, decides; the arm does the I/O.
#[derive(Debug, PartialEq, Eq)]
enum ResultRender {
    /// A utf8 deliverable body — print verbatim to stdout, exit 0.
    Body(String),
    /// A binary deliverable — a one-line metadata note, exit 0.
    BinaryNote(String),
    /// No deliverable: the derived `status` plus the actionable next
    /// gesture (`None` only for a status this binary doesn't recognise),
    /// and a non-zero exit. The cut that kills the bare 404.
    NotReady {
        status: String,
        hint: Option<String>,
    },
}

/// Seconds elapsed since the molecule's last relevant timestamp, for the
/// "Xs ago" / "for Xs" in the hint. `stalled` measures from the last
/// sign of life (`heartbeat_at`, falling back to `tackled_at`); the
/// in-flight states measure from `tackled_at`. A missing or unparseable
/// timestamp — or a negative delta from clock skew — yields `None`, and
/// the hint simply omits the age rather than printing a bogus number.
fn result_age_secs(
    status: &str,
    liveness: Option<&Liveness>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<u64> {
    let lv = liveness?;
    let reference = match status {
        "stalled" => lv.heartbeat_at.as_deref().or(lv.tackled_at.as_deref()),
        _ => lv.tackled_at.as_deref(),
    }?;
    let ts = chrono::DateTime::parse_from_rfc3339(reference)
        .ok()?
        .with_timezone(&chrono::Utc);
    u64::try_from((now - ts).num_seconds()).ok()
}

/// Decide what `result` should print for a human. When a deliverable is
/// present, hand it over; otherwise consume `result_status` (falling back
/// to the raw lifecycle `status` for a pre-C1 server) and pose the next
/// gesture. NEVER a bare 404, never silence.
fn render_result(
    env: &ResultEnvelope,
    name: &str,
    id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> ResultRender {
    if let Some(result) = &env.result {
        return if result.encoding == "utf8" {
            ResultRender::Body(result.content.clone())
        } else {
            ResultRender::BinaryNote(format!(
                "binary result: source={} content_type={} size={} (use --json for base64 body)",
                result.source, result.content_type, result.size_bytes
            ))
        };
    }
    let status = env.result_status.as_deref().unwrap_or(&env.status);
    let age = result_age_secs(status, env.liveness.as_ref(), now);
    let hint = hints::for_result_status(status, name, id, age);
    ResultRender::NotReady {
        status: status.to_owned(),
        hint,
    }
}

/// The basename the operator actually typed (`cosmon-remote`, or the
/// `cosmon` alias the installer poses). Help and usage render under
/// the invoked name: the long name is the
/// contract, the short one is the product face; both are the same
/// binary.
fn invoked_name() -> String {
    std::env::args_os()
        .next()
        .as_deref()
        .map(std::path::Path::new)
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map_or_else(|| "cosmon-remote".to_owned(), str::to_owned)
}

/// The fully-assembled canonical clap tree (name `cosmon-remote`, no
/// invoked-name override). Single spine for parsing, `--help`, and the
/// committed man page — the "one source, two readers" pattern of
/// `cosmon-cli::build_cli`.
#[must_use]
fn build_cli() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}

/// Implementation of the hidden `__man-page` subcommand: render the
/// man page from the live clap tree via [`clap_mangen`] and write it
/// to stdout. `tests/help_goldens.rs::man_page_matches_committed`
/// compares this output byte-for-byte to the committed
/// `man/cosmon-remote.1` — the man page can never drift from the tree.
/// Always rendered under the canonical name: the committed artifact
/// must not depend on whether the operator typed `cosmon` or
/// `cosmon-remote`.
fn print_man_page() -> Result<()> {
    use std::io::Write as _;

    let man = clap_mangen::Man::new(build_cli());
    let mut buf: Vec<u8> = Vec::new();
    man.render(&mut buf)
        .map_err(|e| Error::Config(format!("render man page: {e}")))?;
    std::io::stdout()
        .lock()
        .write_all(&buf)
        .map_err(|e| Error::Config(format!("write man page: {e}")))?;
    Ok(())
}

async fn run() -> Result<()> {
    let name = invoked_name();
    // Render usage, help and the golden-path epilogue under the invoked
    // name (P4 — one name, sourced from `invoked_name()`). `build_cli`
    // and the man page keep the canonical `cosmon-remote`.
    let matches = build_cli()
        .name(name.clone())
        .bin_name(name.clone())
        .after_help(root_help::after_help(&name))
        .after_long_help(root_help::after_long_help(&name))
        .get_matches();
    let cli = match <Cli as clap::FromArgMatches>::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };

    // Introspection plumbing — no profile, no network, no state.
    if matches!(cli.cmd, Cmd::ManPage) {
        return print_man_page();
    }

    let store = ProfileStore::default_location()?;

    let profile_flag = cli.profile.clone();
    let result = dispatch(cli, &store).await;

    // Passive opt-out remontée (delib-20260610-9a0c C3): when the
    // failure predicts abandonment, queue `request_id + error code`
    // for the next successful request and say so in ONE line. The
    // engaged client cuts with `config set phone-home off`; the
    // abandoning client does nothing — and that inaction is exactly
    // the signal the patrouille-abandon needs.
    if let Err(err) = &result {
        let enabled_for = store
            .resolve(profile_flag.as_deref())
            .map(|(_, p)| (p.phone_home, p.host.trim_end_matches('/').to_owned()))
            .ok();
        if let (Some((enabled, host)), Some(dir)) = (enabled_for, phone_home::dir()) {
            if let Some(line) = phone_home::on_failure(
                &dir,
                &host,
                enabled,
                &invoked_name(),
                err,
                chrono::Utc::now(),
            ) {
                eprintln!("{line}");
            }
        }
    }
    result
}

#[allow(clippy::too_many_lines)] // exhaustive match on every Cmd variant
async fn dispatch(cli: Cli, store: &ProfileStore) -> Result<()> {
    match cli.cmd {
        Cmd::Config { sub } => run_config(store, sub, cli.json),
        Cmd::Do {
            topic,
            formula,
            kind,
            vars,
            tags,
            yes,
            follow_timeout,
            poll_interval,
            no_events,
        } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            let mut variables = parse_vars(&vars)?;
            variables.insert("topic".into(), topic);
            let opts = cosmon_remote::do_flow::DoOptions {
                formula,
                kind,
                variables,
                tags,
                assume_yes: yes,
                poll_interval: std::time::Duration::from_secs(poll_interval.max(1)),
                poll_timeout: std::time::Duration::from_secs(follow_timeout),
                follow_events: !no_events,
            };
            // `run_do_cmd` wants the store by value (it remembers the
            // credit-guard answer); the store is cheap to construct.
            run_do_cmd(
                &profile,
                ProfileStore::default_location()?,
                opts,
                cli.token,
                cli.json,
            )
            .await
        }
        Cmd::Run {
            topic,
            formula,
            kind,
            vars,
            tags,
            yes,
            follow_timeout,
            poll_interval,
            no_events,
        } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            let mut variables = parse_vars(&vars)?;
            variables.insert("topic".into(), topic);
            let opts = cosmon_remote::do_flow::DoOptions {
                formula,
                kind,
                variables,
                tags,
                assume_yes: yes,
                poll_interval: std::time::Duration::from_secs(poll_interval.max(1)),
                poll_timeout: std::time::Duration::from_secs(follow_timeout),
                follow_events: !no_events,
            };
            run_run_cmd(
                &profile,
                ProfileStore::default_location()?,
                opts,
                cli.token,
                cli.json,
            )
            .await
        }
        Cmd::Doctor => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            let report = doctor::run(&profile).await;
            if cli.json {
                print_json(true, &serde_json::to_value(&report)?);
            } else {
                render_doctor(&report);
            }
            if !report.healthy() {
                // Red checks → non-zero exit so install.sh and harnesses
                // can read the verdict without parsing the rendering.
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::Healthz => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            let client = Client::new_unchecked(&profile, None)?;
            let body = client.healthz().await?;
            print_json(cli.json, &body);
            Ok(())
        }
        Cmd::Auth { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_auth(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Login => {
            let (name, profile) = store.resolve(cli.profile.as_deref())?;
            run_login(store, &name, &profile, cli.json).await
        }
        Cmd::Logout => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_logout(&profile, cli.json)
        }
        Cmd::Molecule { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_molecule(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Avatar { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_avatar(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Artifact { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_artifact(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Events { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_events(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Quota => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_quota(&profile, cli.token, cli.json).await
        }
        Cmd::Noyaux { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_noyaux(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Workers { sub } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_workers(&profile, sub, cli.token, cli.json).await
        }
        Cmd::Converse {
            avatar_id,
            message,
            message_json,
            kind,
            hop,
        } => {
            let (_, profile) = store.resolve(cli.profile.as_deref())?;
            run_converse(
                &profile,
                &avatar_id,
                &message,
                message_json,
                kind,
                hop,
                cli.token,
                cli.json,
            )
            .await
        }
        // Handled before profile resolution above.
        Cmd::ManPage => unreachable!("__man-page short-circuits before the store"),
    }
}

/// Top-level `converse` dispatch (the conversational channel). The
/// scope is minted from the canon line (`cosmon:pilote:converse`);
/// output is the full response envelope as JSON — a wire-mirror, not
/// a renderer, like the instance-lifecycle verbs.
#[allow(clippy::too_many_arguments)]
async fn run_converse(
    profile: &Profile,
    avatar_id: &str,
    message: &str,
    message_json: bool,
    kind: ConverseKindArg,
    hop: u32,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let route = canon::POST_V1_AVATAR_CONVERSE;
    let message_value = if message_json {
        serde_json::from_str(message)?
    } else {
        serde_json::Value::String(message.to_owned())
    };
    let client = client_for(profile, token, &route.scopes()).await?;
    let env = client
        .converse(avatar_id, &message_value, kind.as_wire(), hop)
        .await?;
    print_json(json, &env);
    Ok(())
}

/// `cosmon-remote noyaux list` — renders the typed
/// [`cosmon_remote::client::NoyauxResponse`] either as JSON (when
/// `--json` is set) or as a small human table.
async fn run_noyaux(
    profile: &Profile,
    sub: NoyauxCmd,
    token_override: Option<String>,
    json: bool,
) -> Result<()> {
    // `/v1/noyaux` is a discovery surface (no scope check); the auth
    // scope catalog matches `/v1/auth/me` — bind only `openid` so the
    // mint succeeds against IdPs that do not vend cosmon-specific
    // scopes.
    let client = client_for(profile, token_override, &auth_scopes()).await?;
    match sub {
        NoyauxCmd::List => {
            let resp = client.noyaux().await?;
            if json {
                print_json(true, &serde_json::to_value(&resp)?);
                return Ok(());
            }
            render_noyaux_table(&resp);
        }
    }
    Ok(())
}

/// Render the noyaux list as a 3-column aligned table on stdout.
/// ASCII-only — same convention as [`render_quota_table`].
fn render_noyaux_table(resp: &cosmon_remote::client::NoyauxResponse) {
    if resp.noyaux.is_empty() {
        println!("no noyaux bound to this principal");
        return;
    }
    let id_width = resp
        .noyaux
        .iter()
        .map(|e| e.id.len())
        .max()
        .unwrap_or(0)
        .max(7);
    let count_width = resp
        .noyaux
        .iter()
        .map(|e| e.binding_count.to_string().len())
        .max()
        .unwrap_or(0)
        .max(8);
    println!(
        "{noyau:<id$}  {bindings:>count$}  galaxies_root",
        noyau = "noyau",
        bindings = "bindings",
        id = id_width,
        count = count_width,
    );
    for entry in &resp.noyaux {
        println!(
            "{:<id$}  {:>count$}  {}",
            entry.id,
            entry.binding_count,
            entry.galaxies_root,
            id = id_width,
            count = count_width,
        );
    }
}

/// `cosmon-remote quota` — renders the typed
/// [`cosmon_remote::client::QuotaResponse`] either as JSON (when
/// `--json` is set) or as a small human table.
async fn run_quota(profile: &Profile, token_override: Option<String>, json: bool) -> Result<()> {
    let client = client_for(profile, token_override, &molecule_scopes()).await?;
    let snap = client.quota().await?;
    if json {
        print_json(true, &serde_json::to_value(&snap)?);
        return Ok(());
    }
    render_quota_table(&snap);
    Ok(())
}

/// `cosmon-remote workers list` — renders the typed
/// [`cosmon_remote::client::WorkersResponse`] either as JSON (with
/// `--json`) or as a one-row-per-worker table.
async fn run_workers(
    profile: &Profile,
    sub: WorkersCmd,
    token_override: Option<String>,
    json: bool,
) -> Result<()> {
    let client = client_for(profile, token_override, &worker_read_scopes()).await?;
    match sub {
        WorkersCmd::List => {
            let resp = client.workers().await?;
            if json {
                print_json(true, &serde_json::to_value(&resp)?);
                return Ok(());
            }
            render_workers_table(&resp);
            Ok(())
        }
    }
}

/// Render the workers list as an ASCII table. Kept deliberately
/// minimal — one line of header, one line per worker.
fn render_workers_table(resp: &cosmon_remote::client::WorkersResponse) {
    println!(
        "workers (count: {}, request_id: {})",
        resp.count, resp.request_id
    );
    if resp.workers.is_empty() {
        println!("  (no active workers in this noyau)");
        return;
    }
    println!(
        "  {:<24} {:<20} {:<20} {:<8} tmux_session",
        "molecule_id", "session_name", "started_at", "pid",
    );
    for w in &resp.workers {
        let pid = w.pid.map_or_else(|| "-".to_owned(), |p| p.to_string());
        println!(
            "  {:<24} {:<20} {:<20} {:<8} {}",
            w.molecule_id, w.session_name, w.started_at, pid, w.tmux_session,
        );
    }
}

/// Render the rate-limit snapshot as a 4-row aligned table on stdout.
/// Kept deliberately ASCII-only so `> table.txt | column -t` works
/// across the operator's shells.
fn render_quota_table(snap: &cosmon_remote::client::QuotaResponse) {
    println!("rate-limit snapshot (request_id: {})", snap.request_id);
    println!("  burst capacity     : {:>6}", snap.limits.burst_capacity);
    println!(
        "  leak per minute    : {:>6.1}",
        snap.limits.leak_per_minute
    );
    println!("  leak per hour      : {:>6.1}", snap.limits.leak_per_hour);
    println!("  current level      : {:>6.2}", snap.current.bucket_level);
    println!("  remaining (floor)  : {:>6}", snap.remaining);
    println!("  reset at           : {}", snap.reset_at);
}

/// Render the doctor report: one line per named check, green ✓ / red ✗
/// / grey ∅ (skipped) / yellow ? (unknown), the repair command indented
/// under every line that carries one.
fn render_doctor(report: &doctor::DoctorReport) {
    for check in &report.checks {
        let (mark, color) = match check.outcome {
            doctor::Outcome::Pass => ("✓", "\x1b[1;32m"),
            doctor::Outcome::Fail => ("✗", "\x1b[1;31m"),
            doctor::Outcome::Skipped => ("∅", "\x1b[2m"),
            doctor::Outcome::Unknown => ("?", "\x1b[1;33m"),
        };
        println!("{color}{mark}\x1b[0m {:<20} {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            println!("  {:<20} fix: {fix}", "");
        }
    }
    if report.healthy() {
        println!("\nall green — ready to work.");
    }
}

fn run_config(store: &ProfileStore, sub: ConfigCmd, json: bool) -> Result<()> {
    // The remediation lines below are copy-paste commands; they must
    // name the binary the operator actually invoked (P4 — sourced from
    // `invoked_name()`, never hand-pinned).
    let bin = invoked_name();
    match sub {
        ConfigCmd::Init { name, host } => {
            let profile = Profile::from_host(host);
            store.write_profile(&name, &profile)?;
            // First profile created → also become the default.
            let mut top = store.read_top()?;
            if top.default_profile.is_none() {
                top.default_profile = Some(name.clone());
                store.write_top(&top)?;
            }
            if json {
                print_json(true, &serde_json::json!({"profile": name}));
            } else {
                println!(
                    "profile {name:?} initialised → {}",
                    store.profile_path(&name).display()
                );
                println!("Next: {bin} config set sub <X>");
                println!("      {bin} config set aud <Y>");
                println!("      {bin} config set oidc-url <Z>");
            }
        }
        ConfigCmd::Set { key, value } => {
            let name = store.resolve_name(None)?;
            let mut profile = store.read_profile(&name)?;
            profile.set(&key, value)?;
            store.write_profile(&name, &profile)?;
            if json {
                print_json(true, &serde_json::to_value(&profile)?);
            } else {
                println!("profile {name:?} updated ({key})");
            }
        }
        ConfigCmd::Show => {
            let (name, profile) = store.resolve(None)?;
            if json {
                print_json(
                    true,
                    &serde_json::json!({"profile": name, "config": profile}),
                );
            } else {
                render_profile(&name, &profile);
            }
        }
        ConfigCmd::Use { name } => {
            // Make sure the named profile exists before flipping the
            // default. Mutate the existing top config so unrelated
            // fields (credit_guard_acknowledged) survive the switch.
            let _ = store.read_profile(&name)?;
            let mut top = store.read_top()?;
            top.default_profile = Some(name.clone());
            store.write_top(&top)?;
            if json {
                print_json(true, &serde_json::json!({"default_profile": name}));
            } else {
                println!("default profile is now {name:?}");
            }
        }
        ConfigCmd::List => {
            let names = store.list_profiles()?;
            let current = store.read_top()?.default_profile;
            if json {
                print_json(
                    true,
                    &serde_json::json!({"profiles": names, "default": current}),
                );
            } else if names.is_empty() {
                println!("no profiles yet — run `{bin} config init <name> <host>`");
            } else {
                for n in names {
                    let marker = if Some(&n) == current.as_ref() {
                        "*"
                    } else {
                        " "
                    };
                    println!("{marker} {n}");
                }
            }
        }
    }
    Ok(())
}

async fn run_auth(
    profile: &Profile,
    sub: AuthCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let client = client_for(profile, token, &auth_scopes()).await?;
    match sub {
        AuthCmd::Login { email, code } => {
            let start = client.auth_start().await?;
            if !json {
                println!("session_id: {}", start.session_id);
            }
            let email_resp = client.auth_email(&start.session_id, &email).await?;
            let code = match code {
                Some(c) => pkce::validate_code(&c)?,
                None => pkce::prompt_for_code(&email_resp.verification_url)?,
            };
            let confirm = client.auth_confirm(&start.session_id, &code).await?;
            if json {
                print_json(
                    true,
                    &serde_json::json!({
                        "session_id": start.session_id,
                        "confirm": confirm,
                    }),
                );
            } else {
                println!("login OK ({confirm})");
            }
        }
        AuthCmd::Status { session_id } => {
            let status = client.auth_status(&session_id).await?;
            if json {
                print_json(true, &serde_json::to_value(&status)?);
            } else {
                println!(
                    "state: {}{}",
                    status.state,
                    status
                        .account_email
                        .as_ref()
                        .map(|e| format!("  ({e})"))
                        .unwrap_or_default()
                );
            }
        }
        AuthCmd::Logout { session_id } => {
            let resp = client.auth_delete(&session_id).await?;
            print_json(json, &resp);
        }
        AuthCmd::Me => {
            let me = client.auth_me().await?;
            if json {
                print_json(true, &serde_json::to_value(&me)?);
            } else {
                println!("sub:        {}", me.sub);
                println!("aud:        {}", me.aud.join(", "));
                println!("issuer:     {}", me.issuer);
                println!("noyau:      {}", me.noyau.as_deref().unwrap_or("<unbound>"));
                println!("expires_at: {}", me.expires_at);
                if me.scopes.is_empty() {
                    println!("scopes:     <none>");
                } else {
                    println!("scopes:     {}", me.scopes.join(" "));
                }
                // The two badges (smithy C1): this command shows the
                // tenant badge — also surface the worker's, when the
                // server publishes it.
                match me.claude_credentials_present {
                    Some(true) => println!("worker:     claude connected"),
                    Some(false) => println!(
                        "worker:     claude NOT connected — `{} auth login` \
                         required before any tackle",
                        invoked_name()
                    ),
                    None => {}
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_molecule(
    profile: &Profile,
    sub: MoleculeCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    // The molecule family mints read+write; `tackle` and `run`
    // additionally require the canon's composed scope (write+spawn —
    // the grid the pre-fusion client under-minted, 403ing every
    // remote tackle; a drain spawns workers too).
    let mut scopes = molecule_scopes();
    let composed = match &sub {
        MoleculeCmd::Tackle { .. } => Some(canon::POST_V1_MOLECULES_ID_TACKLE.scopes()),
        MoleculeCmd::Run { .. } => Some(canon::POST_V1_MOLECULES_ID_RUN.scopes()),
        _ => None,
    };
    if let Some(extras) = composed {
        for extra in extras {
            if !scopes.contains(&extra) {
                scopes.push(extra);
            }
        }
    }
    let client = client_for(profile, token, &scopes).await?;
    match sub {
        MoleculeCmd::Nucleate {
            formula,
            topic,
            description,
            vars,
            kind,
            tags,
        } => {
            let mut variables = parse_vars(&vars)?;
            if let Some(t) = topic {
                variables.insert("topic".into(), t);
            }
            if let Some(d) = description {
                variables.insert("description".into(), d);
            }
            let body = NucleateRequest {
                formula,
                kind,
                variables,
                tags,
            };
            let env = client.nucleate(&body).await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                print_nucleate_truth(&env.molecule);
            }
        }
        MoleculeCmd::List {
            status,
            kind,
            tag,
            fleet,
        } => {
            let filters = ListFilters {
                status,
                kind,
                tag,
                fleet,
            };
            let env = client.list_molecules(&filters).await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                for m in env.molecules() {
                    println!("{:<32}  {:<14}  {}", m.id, m.status, m.kind_label());
                }
            }
        }
        MoleculeCmd::Get { id } => {
            let env = client.get_molecule(&id).await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                println!("id:     {}", env.molecule.id);
                println!("kind:   {}", env.molecule.kind_label());
                println!("status: {}", env.molecule.status);
            }
        }
        MoleculeCmd::Result { id } => {
            let env = client.get_result(&id).await?;
            if json {
                // `--json` hands the whole envelope (status, result_status,
                // liveness, result) straight through — the consumer
                // decides. Exit 0: the JSON IS the answer.
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                match render_result(&env, &invoked_name(), &id, chrono::Utc::now()) {
                    // The whole point of the route: hand the tenant their
                    // deliverable, verbatim to stdout.
                    ResultRender::Body(body) => print!("{body}"),
                    // Binary deliverable — don't spray bytes at the
                    // terminal; report what is there, let them use --json.
                    ResultRender::BinaryNote(note) => println!("{note}"),
                    // No deliverable. The cut (C4): never a bare 404,
                    // never silence — name the status and pose the next
                    // gesture in the same breath, then exit non-zero.
                    ResultRender::NotReady { status, hint } => {
                        eprintln!("no deliverable yet (status: {status})");
                        if let Some(line) = hint {
                            for l in line.lines() {
                                eprintln!("  ↳ {}", l.trim_start());
                            }
                        }
                        return Err(Error::NoDeliverable { status });
                    }
                }
            }
        }
        MoleculeCmd::Tackle { id } => {
            let env = client.tackle(&id).await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                println!(
                    "tackled: {}  worker={}",
                    env.tackle.molecule_id,
                    env.tackle.worker_session.as_deref().unwrap_or("-")
                );
            }
        }
        MoleculeCmd::Run { id } => {
            let env = client.run(&id).await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                println!(
                    "drain started on {} (budget={}, max_depth={}, max_molecules={}, timeout={}s)",
                    env.drain.root,
                    env.drain.bounds.budget,
                    env.drain.bounds.max_depth,
                    env.drain.bounds.max_molecules,
                    env.drain.timeout_secs,
                );
                println!(
                    "follow: events stream --molecule-id {}  (drain.terminated names the exit)",
                    env.drain.root,
                );
            }
        }
        MoleculeCmd::Collapse { id, reason, cause } => {
            let body = CollapseRequest { reason, cause };
            let body = client.collapse(&id, &body).await?;
            print_json(json, &body);
        }
        MoleculeCmd::Freeze { id, reason } => {
            let body = client.freeze(&id, Some(&reason)).await?;
            print_json(json, &body);
        }
        MoleculeCmd::Thaw { id, reason } => {
            let body = client.thaw(&id, Some(&reason)).await?;
            print_json(json, &body);
        }
        MoleculeCmd::Stuck { id, reason } => {
            let body = client.stuck(&id, &ReasonRequest { reason }).await?;
            print_json(json, &body);
        }
        MoleculeCmd::Tag { id, add, remove } => {
            if add.is_empty() && remove.is_empty() {
                return Err(Error::Config(
                    "tag requires at least one --add or --remove".into(),
                ));
            }
            let body = client.tag(&id, &TagRequest { add, remove }).await?;
            print_json(json, &body);
        }
    }
    Ok(())
}

/// D-AVATAR lifecycle dispatch. Scopes are minted per route from the
/// canon (`world:observe` for reads, `pilote:converse` for binds) —
/// no hand scope list to drift.
async fn run_avatar(
    profile: &Profile,
    sub: AvatarCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let env = match &sub {
        AvatarCmd::Status { instance_id } => {
            let route = canon::GET_V1_AVATAR_INSTANCE_ID_STATUS;
            let client = client_for(profile, token, &route.scopes()).await?;
            client.avatar_status(instance_id).await?
        }
        AvatarCmd::Incarnate {
            instance_id,
            pilote,
            tenant,
            juridiction,
        } => {
            let route = canon::POST_V1_AVATAR_INSTANCE_ID_INCARNATE;
            let client = client_for(profile, token, &route.scopes()).await?;
            client
                .avatar_incarnate(instance_id, pilote, tenant, juridiction)
                .await?
        }
        AvatarCmd::Grant {
            instance_id,
            canal,
            target,
        } => {
            let route = canon::POST_V1_AVATAR_INSTANCE_ID_GRANT;
            let client = client_for(profile, token, &route.scopes()).await?;
            client.avatar_grant(instance_id, canal, target).await?
        }
        AvatarCmd::Audit { instance_id } => {
            let route = canon::GET_V1_AVATAR_INSTANCE_ID_AUDIT;
            let client = client_for(profile, token, &route.scopes()).await?;
            client.avatar_audit(instance_id).await?
        }
        AvatarCmd::MouldInfo { instance_id } => {
            let route = canon::GET_V1_AVATAR_INSTANCE_ID_MOULD_INFO;
            let client = client_for(profile, token, &route.scopes()).await?;
            client.avatar_mould_info(instance_id).await?
        }
    };
    print_json(json, &env);
    Ok(())
}

async fn run_artifact(
    profile: &Profile,
    sub: ArtifactCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let client = client_for(profile, token, &molecule_scopes()).await?;
    match sub {
        ArtifactCmd::List { mol_id } => {
            let m = client.list_artifacts(&mol_id).await?;
            if json {
                print_json(true, &serde_json::to_value(&m)?);
            } else if m.artifacts.is_empty() {
                println!("no artifacts for {mol_id}");
            } else {
                println!("molecule: {}", m.molecule_id);
                for a in &m.artifacts {
                    println!(
                        "  {:<32} {:>10} B  {}  {}",
                        a.name, a.size_bytes, a.content_type, a.token
                    );
                }
            }
        }
        ArtifactCmd::Get {
            mol_id,
            artifact_token: art_token,
            out,
        } => {
            let dest = out.unwrap_or_else(|| {
                let base = profile
                    .artifacts_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("cosmon-artifacts"));
                base.join(&mol_id).join(&art_token)
            });
            let fetched = client.fetch_artifact(&mol_id, &art_token, &dest).await?;
            if json {
                print_json(
                    true,
                    &serde_json::json!({
                        "dest": fetched.dest,
                        "bytes": fetched.bytes,
                        "content_type": fetched.content_type,
                        "etag": fetched.etag,
                    }),
                );
            } else {
                println!(
                    "wrote {} ({} B, content-type: {})",
                    fetched.dest.display(),
                    fetched.bytes,
                    fetched.content_type.as_deref().unwrap_or("?"),
                );
            }
        }
        ArtifactCmd::Push {
            mol_id,
            name,
            file,
            content_type,
            if_match,
        } => {
            let env = client
                .push_artifact(
                    &mol_id,
                    &name,
                    &file,
                    content_type.as_deref(),
                    if_match.as_deref(),
                )
                .await?;
            if json {
                print_json(true, &serde_json::to_value(&env)?);
            } else {
                println!(
                    "pushed: {}  {} B  digest={}",
                    env.artifact.name, env.artifact.size_bytes, env.artifact.integrity.hex
                );
            }
        }
    }
    Ok(())
}

/// Drive `GET /v1/events`. Connects, streams chunks to stdout (one
/// SSE event per pretty-printed JSON line in `--json` mode, or a
/// compact human shape otherwise), and exits when the connection
/// closes (server kicks us off, network error, or operator Ctrl-C).
async fn run_events(
    profile: &Profile,
    sub: EventsCmd,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    let client = client_for(profile, token, &events_scopes()).await?;
    match sub {
        EventsCmd::Stream {
            molecule_id,
            last_event_id,
        } => {
            client
                .events_stream(molecule_id.as_deref(), last_event_id, |evt| {
                    if json {
                        // One JSON object per line — easy to pipe
                        // through `jq`.
                        match serde_json::to_string(&evt) {
                            Ok(s) => println!("{s}"),
                            Err(e) => eprintln!("warn: failed to render event: {e}"),
                        }
                    } else {
                        let molecule = evt
                            .data_obj()
                            .as_ref()
                            .and_then(|v| v.get("molecule_id"))
                            .and_then(|v| v.as_str())
                            .map_or_else(|| "-".to_owned(), ToOwned::to_owned);
                        println!(
                            "[{}] {} molecule={} data={}",
                            evt.id.as_deref().unwrap_or("-"),
                            evt.event,
                            molecule,
                            evt.data,
                        );
                    }
                })
                .await?;
        }
    }
    Ok(())
}

/// `cosmon do "<topic>"` — drive the client-side composition of
/// [`cosmon_remote::do_flow::run_do`] with the real interactive edges:
/// stdin for the credit guard, the profile store for the one-time
/// acknowledgment, stdout for progress.
async fn run_do_cmd(
    profile: &Profile,
    mut store: ProfileStore,
    opts: cosmon_remote::do_flow::DoOptions,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    // Union of everything the composition dials: nucleate (write),
    // tackle (write+spawn), observe (read), events tail (subscribe).
    let mut scopes = molecule_scopes();
    for extra in canon::POST_V1_MOLECULES_ID_TACKLE.scopes() {
        if !scopes.contains(&extra) {
            scopes.push(extra);
        }
    }
    if opts.follow_events {
        for extra in events_scopes() {
            if !scopes.contains(&extra) {
                scopes.push(extra);
            }
        }
    }
    let client = client_for(profile, token, &scopes).await?;

    let confirm = |prompt: &str| -> std::io::Result<bool> {
        use std::io::Write as _;
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let a = answer.trim().to_ascii_lowercase();
        Ok(a == "y" || a == "yes")
    };

    let outcome = cosmon_remote::do_flow::run_do(&client, opts, &mut store, confirm, |line| {
        println!("{line}");
    })
    .await?;

    if json {
        print_json(
            true,
            &serde_json::json!({
                "molecule_id": outcome.molecule_id,
                "terminal_status": outcome.terminal_status,
                "guard_shown": outcome.guard_shown,
            }),
        );
        return Ok(());
    }
    match outcome.terminal_status.as_deref() {
        Some("completed") => println!(
            "done — fetch the deliverable: molecule result {}",
            outcome.molecule_id
        ),
        Some(status) => println!(
            "terminal status `{status}` — inspect: molecule get {}",
            outcome.molecule_id
        ),
        None => println!(
            "still running — pick it up later: molecule result {}",
            outcome.molecule_id
        ),
    }
    Ok(())
}

/// `cosmon-remote run` — the `do` composition bracketed by two
/// `GET /v1/quota` reads, so the operator sees the cost THIS run charged
/// against their bucket. Shares `do`'s scope union, credit guard, and
/// progress edges; the quota bracket is best-effort and never fails the
/// run (see [`cosmon_remote::cost`]).
async fn run_run_cmd(
    profile: &Profile,
    mut store: ProfileStore,
    opts: cosmon_remote::do_flow::DoOptions,
    token: Option<String>,
    json: bool,
) -> Result<()> {
    // Same union as `do`: nucleate (write), tackle (write+spawn),
    // observe (read), events tail (subscribe). The quota read needs only
    // `cosmon:molecule:read`, already in `molecule_scopes()`.
    let mut scopes = molecule_scopes();
    for extra in canon::POST_V1_MOLECULES_ID_TACKLE.scopes() {
        if !scopes.contains(&extra) {
            scopes.push(extra);
        }
    }
    if opts.follow_events {
        for extra in events_scopes() {
            if !scopes.contains(&extra) {
                scopes.push(extra);
            }
        }
    }
    let client = client_for(profile, token, &scopes).await?;

    let confirm = |prompt: &str| -> std::io::Result<bool> {
        use std::io::Write as _;
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let a = answer.trim().to_ascii_lowercase();
        Ok(a == "y" || a == "yes")
    };

    let outcome = cosmon_remote::cost::run_with_cost(&client, opts, &mut store, confirm, |line| {
        println!("{line}");
    })
    .await?;

    let do_outcome = &outcome.do_outcome;
    if json {
        print_json(
            true,
            &serde_json::json!({
                "molecule_id": do_outcome.molecule_id,
                "terminal_status": do_outcome.terminal_status,
                "guard_shown": do_outcome.guard_shown,
                "cost_delta": outcome.cost,
            }),
        );
        return Ok(());
    }
    match do_outcome.terminal_status.as_deref() {
        Some("completed") => println!(
            "done — fetch the deliverable: molecule result {}",
            do_outcome.molecule_id
        ),
        Some(status) => println!(
            "terminal status `{status}` — inspect: molecule get {}",
            do_outcome.molecule_id
        ),
        None => println!(
            "still running — pick it up later: molecule result {}",
            do_outcome.molecule_id
        ),
    }
    match &outcome.cost {
        Some(delta) => println!("{}", delta.render()),
        None => println!(
            "attributed cost: unavailable (quota snapshot failed — the work still ran; \
             read it manually with `quota`)"
        ),
    }
    Ok(())
}

/// Tell the truth about what `nucleate` just created: a 201 is NOT
/// work in motion. Nothing descends until a
/// `tackle` — and when the formula wired pending children behind the
/// root, name them instead of staying silent (the current lie is the
/// silence).
fn print_nucleate_truth(molecule: &cosmon_remote::client::MoleculeView) {
    // The tackle lines are copy-paste commands — name the invoked
    // binary, never a hand-pinned long name (P4).
    let bin = invoked_name();
    println!("nucleated: {}  (status: {})", molecule.id, molecule.status);
    let children = pending_children(molecule);
    if children.is_empty() {
        println!("nothing starts on its own — the molecule awaits your gesture:");
        println!(
            "  {bin} molecule tackle {}   (launches a worker — costs credit)",
            molecule.id
        );
    } else {
        println!(
            "{} pending child(ren) created behind this molecule: {}",
            children.len(),
            children.join(", ")
        );
        println!("nothing starts on its own — tackle the root, then each unblocked child:");
        println!(
            "  {bin} molecule tackle {}   (each tackle launches a worker — costs credit)",
            molecule.id
        );
        println!("  (automatic polymerisation does not exist avatar-side yet)");
    }
}

/// Children wired behind a molecule at nucleation: the wire's
/// `typed_links` entries with `rel == "blocks"` (this molecule blocks
/// the target ⇒ the target is a pending child waiting on it). Reads
/// the signal the server already emits — never inferred client-side.
fn pending_children(molecule: &cosmon_remote::client::MoleculeView) -> Vec<String> {
    molecule
        .extra
        .get("typed_links")
        .and_then(|v| v.as_array())
        .map(|links| {
            links
                .iter()
                .filter(|l| l.get("rel").and_then(|r| r.as_str()) == Some("blocks"))
                .filter_map(|l| l.get("target").and_then(|t| t.as_str()))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_vars(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| Error::Config(format!("--var expects KEY=VALUE, got {pair:?}")))?;
        out.insert(k.to_owned(), v.to_owned());
    }
    Ok(out)
}

fn render_profile(name: &str, p: &Profile) {
    println!("profile:      {name}");
    println!("host:         {}", p.host);
    println!("sub:          {}", p.sub);
    println!("aud:          {}", p.aud);
    println!("oidc_url:     {}", p.oidc_url);
    println!("noyau:        {}", p.noyau.as_deref().unwrap_or("-"));
    println!("timeout_secs: {}", p.timeout_secs);
    println!("scopes:");
    for s in &p.scopes {
        println!("  - {s}");
    }
    if let Some(a) = &p.artifacts_dir {
        println!("artifacts_dir: {}", a.display());
    }
    if let Err(e) = p.check_ready() {
        eprintln!("\n⚠️  {e}");
    }
}

fn print_json(json: bool, body: &serde_json::Value) {
    if json {
        match serde_json::to_string_pretty(body) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to render JSON: {e}"),
        }
    } else {
        // No human-friendly form for opaque envelopes — just pretty-print.
        match serde_json::to_string_pretty(body) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to render: {e}"),
        }
    }
}

fn molecule_scopes() -> Vec<String> {
    vec![
        "cosmon:molecule:read".into(),
        "cosmon:molecule:write".into(),
    ]
}

fn auth_scopes() -> Vec<String> {
    vec!["cosmon:auth:claude:write".into()]
}

fn events_scopes() -> Vec<String> {
    vec!["cosmon:events:subscribe".into()]
}

fn worker_read_scopes() -> Vec<String> {
    vec!["cosmon:worker:read".into()]
}

/// Construct a [`Client`] for an authenticated call. Token-resolution precedence
/// (delib-20260710-33b7 C2 — the "chaque commande lit le trousseau" seam):
///
/// 1. explicit `--token` flag,
/// 2. `$COSMON_REMOTE_TOKEN` (CI / smoke harness),
/// 3. **the persisted credential + silent refresh** — for real-OIDC profiles
///    (`login` has recorded `issuer` + `client_id`): read the keyring, use the
///    access token directly when valid (zero network), refresh it silently when
///    expiring, and tell the operator to `login` when the refresh is exhausted,
/// 4. the legacy OIDC **mock mint** (`oidc_url/issue`) — unchanged for mock
///    deployments that never ran a real `login`.
async fn client_for(
    profile: &Profile,
    flag_token: Option<String>,
    scopes: &[String],
) -> Result<Client> {
    if let Some(t) = flag_token {
        return Client::new(profile, Some(t));
    }
    if let Ok(t) = std::env::var(ENV_TOKEN) {
        if !t.is_empty() {
            return Client::new(profile, Some(t));
        }
    }

    // Real-OIDC profile: reach for the persisted credential and refresh silently.
    // Proactive refresh happens here (on the 15-min boundary); the returned
    // reactive binding lets a command recover from a residual server `401`
    // (clock drift past the leeway) with a single silent refresh + retry.
    if profile.is_real_oidc() {
        let (token, reauth) = ensure_persisted_token(profile).await?;
        return Ok(Client::new(profile, Some(token))?.with_reauth(reauth));
    }

    // No token in hand — mint one via the deployment's OIDC mock.
    let unauth = Client::new(profile, None)?;
    let minted = unauth.mint_jwt(scopes).await?;
    Ok(unauth.with_token(minted.access_token))
}

/// Read the persisted credential for a real-OIDC `profile`, refreshing silently
/// when the 15-minute access token is expiring, and return `(bearer, reauth)` —
/// a valid bearer plus a [`ReactiveRefresh`] binding the [`Client`] uses to
/// recover from a residual server `401` (clock drift past the leeway) with one
/// more silent refresh. When the store is cold or the refresh token is
/// exhausted, abort with a precise "run `login`" message rather than silently
/// falling through to the mock mint.
async fn ensure_persisted_token(profile: &Profile) -> Result<(String, ReactiveRefresh)> {
    use cosmon_remote::credential::{CredentialKey, CredentialStore};
    use cosmon_remote::oidc::{self, TokenState};

    let issuer = profile
        .issuer
        .as_deref()
        .ok_or_else(|| Error::Config("profile has no recorded issuer; run `login`".into()))?;
    let client_id = profile.effective_client_id().to_owned();
    let key = CredentialKey::new(issuer, &profile.sub, &client_id);
    let store = CredentialStore::detect()?;
    // One HTTP client, reused for the (rare) proactive discovery+refresh and
    // handed to the reactive binding.
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(profile.timeout_secs))
        .build()?;

    let leeway = chrono::Duration::seconds(oidc::REFRESH_LEEWAY_SECS);
    let now = chrono::Utc::now();

    // Fast path first — a valid cached access token needs no network at all.
    let state = match oidc::cached_access(&store, &key, now, leeway)? {
        oidc::CacheState::Fresh(token) => TokenState::Valid(token),
        oidc::CacheState::Cold => TokenState::NeedsLogin,
        oidc::CacheState::Stale(_) => {
            // A refresh is needed: discover the token endpoint (network, only on
            // the 15-minute boundary) and run the single-writer refresh.
            let cfg = oidc::RefreshConfig {
                token_endpoint: oidc::ProviderMetadata::fetch(&http, &profile.oidc_url)
                    .await?
                    .token_endpoint,
                client_id: client_id.clone(),
                // Forgejo single-uses refresh tokens (InvalidateRefreshTokens=true),
                // so an omitted refresh_token in a grant response means the
                // presented one is already spent — never reuse it.
                rotation: oidc::RefreshRotation::Rotating,
            };
            oidc::refresh_credential(&http, &store, &key, &cfg, leeway).await?
        }
    };

    let bearer = match state {
        TokenState::Valid(token) => token.expose().to_owned(),
        TokenState::NeedsLogin => {
            return Err(Error::Config(format!(
                "no valid cosmon credential for profile {:?} — run `cosmon-remote login`",
                profile.sub
            )))
        }
    };
    // The store/key/http move into the reactive binding, which resolves the
    // token endpoint lazily (only on an actual 401) so the fast path stays
    // network-free.
    let reauth = ReactiveRefresh::new(http, store, key, profile.oidc_url.clone(), client_id);
    Ok((bearer, reauth))
}

/// `login` — run the real OAuth2-PKCE browser flow, persist the credential, and
/// record the resolved `issuer` + `client_id` back into the profile so
/// subsequent commands refresh silently offline.
async fn run_login(store: &ProfileStore, name: &str, profile: &Profile, json: bool) -> Result<()> {
    use cosmon_remote::credential::CredentialStore;
    use cosmon_remote::oidc;

    profile.check_ready()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(profile.timeout_secs))
        .build()?;

    // Discover the provider endpoints (standard OIDC) and the provisioned
    // client_id for this profile's audience (cosmon reverse-discovery).
    let endpoints = oidc::discover(
        &http,
        &profile.oidc_url,
        &profile.host,
        &profile.aud,
        profile.scopes.clone(),
    )
    .await?;

    let cred_store = CredentialStore::detect()?;
    let timeout = std::time::Duration::from_secs(oidc::LOGIN_TIMEOUT_SECS);
    let outcome = oidc::login(
        &http,
        &cred_store,
        &endpoints,
        &profile.sub,
        timeout,
        oidc::open_browser,
    )
    .await?;

    // Record issuer + client_id so `client_for` rebuilds the key offline. A
    // changed client_id here is a re-provision — the new value simply wins.
    let mut updated = profile.clone();
    updated.issuer = Some(endpoints.issuer.clone());
    updated.client_id = Some(endpoints.client_id.clone());
    store.write_profile(name, &updated)?;

    if json {
        print_json(
            true,
            &serde_json::json!({
                "ok": true,
                "profile": name,
                "issuer": endpoints.issuer,
                "client_id": endpoints.client_id,
                "backend": outcome.backend.as_str(),
                "expires_at": outcome.expires_at.to_rfc3339(),
            }),
        );
    } else {
        println!(
            "✓ Signed in to {} as {} (credential stored in {}; access token expires {})",
            profile.host,
            profile.sub,
            outcome.backend.as_str(),
            outcome.expires_at.to_rfc3339(),
        );
    }
    Ok(())
}

/// `logout` — forget the persisted cosmon credential for the active profile.
fn run_logout(profile: &Profile, json: bool) -> Result<()> {
    use cosmon_remote::credential::{CredentialKey, CredentialStore};
    use cosmon_remote::oidc;

    let store = CredentialStore::detect()?;
    let client_id = profile.effective_client_id();
    // Delete the recorded issuer's slot when present; otherwise best-effort on
    // the mock label (a no-op if nothing was stored — logout is idempotent).
    let issuer = profile.issuer.as_deref().unwrap_or(&profile.oidc_url);
    let key = CredentialKey::new(issuer, &profile.sub, client_id);
    oidc::logout(&store, &key)?;

    if json {
        print_json(true, &serde_json::json!({"ok": true, "signed_out": true}));
    } else {
        println!("✓ Signed out — the persisted cosmon credential was removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(json: serde_json::Value) -> cosmon_remote::client::MoleculeView {
        serde_json::from_value(json).unwrap()
    }

    /// No `typed_links` on the wire → no children claimed. The truth
    /// line then says "tackle it", never invents a DAG.
    #[test]
    fn pending_children_empty_without_links() {
        let m = view(serde_json::json!({"id": "task-1", "status": "pending"}));
        assert!(pending_children(&m).is_empty());
    }

    /// `rel: blocks` entries are the children pending behind the root —
    /// exactly what the nucleate output must name (the current lie is
    /// the silence).
    #[test]
    fn pending_children_reads_blocks_links() {
        let m = view(serde_json::json!({
            "id": "task-root",
            "status": "pending",
            "typed_links": [
                {"rel": "blocks", "target": "task-child-1"},
                {"rel": "blocks", "target": "task-child-2"},
                {"rel": "blocked_by", "source": "task-up"},
                {"rel": "refines", "target": "idea-9"}
            ]
        }));
        assert_eq!(pending_children(&m), vec!["task-child-1", "task-child-2"]);
    }

    fn artifact_get_parts(cli: Cli) -> (Option<String>, String, String) {
        match cli.cmd {
            Cmd::Artifact {
                sub:
                    ArtifactCmd::Get {
                        mol_id,
                        artifact_token,
                        ..
                    },
            } => (cli.token, mol_id, artifact_token),
            other => panic!("expected artifact get, parsed {other:?}"),
        }
    }

    /// The `artifact get`
    /// positional must never leak into the global `--token` slot —
    /// with no `--token` given, the CLI must mint a JWT (token=None),
    /// not send the artifact token as the bearer.
    #[test]
    fn artifact_get_positional_stays_out_of_global_token() {
        use clap::Parser as _;
        let cli =
            Cli::try_parse_from(["cosmon-remote", "artifact", "get", "task-X", "tok-Y"]).unwrap();
        let (global_token, mol_id, artifact_token) = artifact_get_parts(cli);
        assert_eq!(global_token, None, "positional leaked into global --token");
        assert_eq!(mol_id, "task-X");
        assert_eq!(artifact_token, "tok-Y");
    }

    /// Replay-Dave D2, evidence/21: an explicit `--token GOOD.JWT.HERE`
    /// was overwritten by the positional. The explicit JWT must win.
    #[test]
    fn artifact_get_explicit_token_flag_is_preserved() {
        use clap::Parser as _;
        let cli = Cli::try_parse_from([
            "cosmon-remote",
            "--token",
            "GOOD.JWT.HERE",
            "artifact",
            "get",
            "task-X",
            "tok-Y",
        ])
        .unwrap();
        let (global_token, _, artifact_token) = artifact_get_parts(cli);
        assert_eq!(global_token.as_deref(), Some("GOOD.JWT.HERE"));
        assert_eq!(artifact_token, "tok-Y");
    }

    // ---- C4: `result` rendering consumes result_status -------------

    fn result_env(json: serde_json::Value) -> ResultEnvelope {
        serde_json::from_value(json).unwrap()
    }

    /// A resolved utf8 deliverable is handed over verbatim.
    #[test]
    fn render_result_hands_over_a_utf8_body() {
        let env = result_env(serde_json::json!({
            "request_id": "r-1", "molecule_id": "task-1",
            "status": "completed", "result_status": "ready",
            "result": {
                "source": "result.md", "content_type": "text/markdown",
                "encoding": "utf8", "content": "the deliverable",
                "size_bytes": 15, "integrity": {"algo": "blake3", "hex": "ab"}
            }
        }));
        assert_eq!(
            render_result(&env, "cosmon", "task-1", chrono::Utc::now()),
            ResultRender::Body("the deliverable".into())
        );
    }

    /// The gate: a STALLED molecule (200, `result:null`) is NEVER a bare
    /// 404 and NEVER silence — it names the status and the exact relaunch
    /// gesture under the invoked name.
    #[test]
    fn render_result_on_stalled_poses_the_relaunch_gesture() {
        let env = result_env(serde_json::json!({
            "request_id": "r-1", "molecule_id": "task-2f9a",
            "status": "running", "result_status": "stalled",
            "liveness": {
                "process": {"worker_id": "w-1", "status": "active"},
                "heartbeat_at": "2026-06-14T00:00:00Z",
                "tackled_at": "2026-06-14T00:00:00Z",
                "stale_after_s": 900
            },
            "result": null
        }));
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-14T00:15:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        match render_result(&env, "cosmon", "task-2f9a", now) {
            ResultRender::NotReady { status, hint } => {
                assert_eq!(status, "stalled");
                let hint = hint.expect("stalled carries a hint");
                assert!(
                    hint.contains("cosmon molecule tackle task-2f9a"),
                    "must pose the relaunch gesture, got: {hint}"
                );
                // 15 minutes since the last sign of life.
                assert!(hint.contains("900s"), "age from heartbeat, got: {hint}");
            }
            other => panic!("stalled must render NotReady, got {other:?}"),
        }
    }

    /// `done-no-deliverable` points at the REAL artifact-listing command.
    #[test]
    fn render_result_done_no_deliverable_points_at_artifact_list() {
        let env = result_env(serde_json::json!({
            "request_id": "r-1", "molecule_id": "task-3",
            "status": "completed", "result_status": "done-no-deliverable",
            "result": null
        }));
        match render_result(&env, "cosmon-remote", "task-3", chrono::Utc::now()) {
            ResultRender::NotReady { hint, .. } => {
                assert!(hint.unwrap().contains("cosmon-remote artifact list task-3"));
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }

    /// A pre-C1 server sends no `result_status`; the rendering falls back
    /// to the raw lifecycle `status` and still poses a gesture (no panic,
    /// no bare 404).
    #[test]
    fn render_result_falls_back_to_lifecycle_status() {
        let env = result_env(serde_json::json!({
            "request_id": "r-1", "molecule_id": "task-4",
            "status": "pending", "result": null
        }));
        match render_result(&env, "cosmon", "task-4", chrono::Utc::now()) {
            ResultRender::NotReady { status, hint } => {
                assert_eq!(status, "pending");
                assert!(hint.unwrap().contains("cosmon molecule result task-4"));
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
    }
}
