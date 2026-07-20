// SPDX-License-Identifier: AGPL-3.0-only

//! Cosmon CLI — the single-binary entry point for agent fleet management.
//!
//! Routes subcommands to their handlers, threading global flags through
//! a shared [`cmd::Context`].

use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod dotpath;
mod energy_probe;
mod event_log;
mod llm;
mod mindguard;
mod native;
mod neurion_hint;
mod operator_event;
mod pow;
mod resurrect;
mod root_help;
mod visual;

// Sensorium loader (vital strip) lives in the lib crate so external
// integration tests can compute the strip without invoking the
// binary; the binary re-uses it via the lib.
use cosmon_cli::sensorium;

/// Cosmon — compose, pilot and audit long-haul AI missions where the trace matters.
#[derive(Parser)]
#[command(
    name = "cs",
    // Full build identity, not just the crate version: two repos can
    // (and do) install the same `cs` at the same crate version into
    // `~/.local/bin`; the SHA/dirty/date stamp makes `--version` enough
    // to tell them apart without the hidden `__build-sha` plumbing.
    version = cosmon_cli::long_version(),
    about = "Cosmon — compose, pilot and audit long-haul AI missions where the trace matters.",
    long_about = root_help::LONG_ABOUT,
    after_long_help = root_help::AFTER_LONG_HELP,
    disable_help_subcommand = true
)]
struct Cli {
    /// Path to configuration file
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Output in JSON format
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Display ensemble status — observe the fleet at a glance
    #[command(after_help = cmd::examples::ENSEMBLE)]
    Ensemble(cmd::ensemble::Args),

    /// Nucleate a new molecule from a formula template
    #[command(after_help = cmd::examples::NUCLEATE)]
    Nucleate(Box<cmd::nucleate::Args>),

    /// Observe a molecule's current state and history
    #[command(after_help = cmd::examples::OBSERVE)]
    Observe(cmd::observe::Args),

    /// Inspect and query the `EventV2` event log
    #[command(after_help = cmd::examples::EVENTS, hide = true)]
    Events(cmd::events::Args),

    /// Errors — aggregate molecule-collapse events into one failure overview: what is breaking the fleet, and which molecules are hit
    Errors(cmd::errors::Args),

    /// Evolve a molecule to its next lifecycle state
    #[command(after_help = cmd::examples::EVOLVE)]
    Evolve(cmd::evolve::Args),

    /// Collapse a molecule — terminate with final state recording
    #[command(after_help = cmd::examples::COLLAPSE)]
    Collapse(cmd::collapse::Args),

    /// Complete a molecule — idempotent Active→Completed transition (worker-callable)
    #[command(after_help = cmd::examples::COMPLETE)]
    Complete(cmd::complete::Args),

    /// Decay a molecule into child molecules (1 → N)
    #[command(after_help = cmd::examples::DECAY)]
    Decay(cmd::interaction::DecayArgs),

    /// Merge molecules into a synthesis (N → 1)
    #[command(after_help = cmd::examples::MERGE)]
    Merge(cmd::interaction::MergeArgs),

    /// Transform a molecule's kind (idea → task, etc.)
    #[command(after_help = cmd::examples::TRANSFORM)]
    Transform(cmd::interaction::TransformArgs),

    /// Bootstrap a project-local .cosmon/ directory (creates the target dir if missing)
    #[command(after_help = cmd::examples::INIT)]
    Init(cmd::init::Args),

    /// Inbox — vertical pile of atomic actions awaiting operator decision (cs inbox)
    #[command(after_help = cmd::examples::INBOX)]
    Inbox(cmd::inbox::Args),

    /// Spark — capture a one-line operator intent into the Inbox (ADR-061)
    #[command(after_help = cmd::examples::SPARK)]
    Spark(cmd::spark::Args),

    /// Drop — universal Inbox gesture (hotkey / zsh widget / menubar) → `cs spark`
    #[command(after_help = cmd::examples::DROP)]
    Drop(cmd::drop::Args),

    /// Listen — voice → whisper.cpp → `cs nucleate spark` (MVP)
    #[command(after_help = cmd::examples::LISTEN)]
    Listen(cmd::listen::Args),

    /// Ask — conversational ingress: free-text → resolved molecule (MVP, --experimental)
    #[command(hide = true)]
    Ask(cmd::ask::Args),

    /// Pilot — interactive cognitive pilot REPL over a client-side model (--experimental). --remote pilots an avatar over the §8p wire (ADR-115)
    Pilot(cmd::pilot::Args),

    /// Kill a worker — immediate termination (no state flush)
    #[command(after_help = cmd::examples::KILL)]
    Kill(cmd::kill::Args),

    /// Quench a worker — graceful shutdown with state preservation
    #[command(after_help = cmd::examples::QUENCH)]
    Quench(cmd::quench::Args),

    /// Fleet template discovery and initialization
    #[command(after_help = cmd::examples::FLEET)]
    Fleet(cmd::fleet::Args),

    /// Galaxies — inspect the four-family taxonomy
    #[command(after_help = cmd::examples::GALAXIES)]
    Galaxies(cmd::galaxies::Args),

    /// Mur du Matin — morning fresque of the galaxy cluster
    #[command(after_help = cmd::examples::MUR, hide = true)]
    Mur(cmd::mur::Args),

    /// Motion — live view of "molécules en mouvement" across the cluster
    #[command(after_help = cmd::examples::MOTION, hide = true)]
    Motion(cmd::motion::Args),

    /// Freeze a worker — suspend with state preservation (preemption)
    #[command(after_help = cmd::examples::FREEZE)]
    Freeze(cmd::freeze::Args),

    /// Thaw a worker — resume a frozen worker's Claude session
    #[command(after_help = cmd::examples::THAW)]
    Thaw(cmd::thaw::Args),

    /// Trust — grant this repository permission to run its own formulas/hooks
    /// (the `direnv allow` of cosmon; refuses repo-supplied shell until granted)
    Trust(cmd::trust::Args),

    /// Prime the system — load .cosmon/config.toml and self-check gates
    #[command(after_help = cmd::examples::PRIME)]
    Prime(cmd::prime::Args),

    /// Resume — convenience alias for `cs patrol --propel --molecule <id>`
    #[command(after_help = cmd::examples::RESUME)]
    Resume(cmd::resume::Args),

    /// Resurrect — revive a wrecked (stuck) molecule with a fresh worker
    #[command(hide = true)]
    Resurrect(cmd::resurrect::Args),

    /// Teardown a fleet — gracefully stop all fleet workers
    #[command(after_help = cmd::examples::TEARDOWN)]
    Teardown(cmd::teardown::Args),

    /// Purge dead workers from fleet state (Stopped, Error, Stale)
    #[command(after_help = cmd::examples::PURGE)]
    Purge(cmd::purge::Args),

    /// Migrate the galaxy's memory — legacy flat→fleet, or residence-to-residence
    #[command(after_help = cmd::examples::MIGRATE)]
    Migrate(cmd::migrate::Args),

    /// Patrol the fleet — health checks and anomaly detection
    #[command(after_help = cmd::examples::PATROL)]
    Patrol(cmd::patrol::Args),

    /// Health — read-only molecule-health anomaly catalog, federation-wide (ADR-137 §7)
    #[command(after_help = cmd::examples::HEALTH)]
    Health(cmd::health::Args),

    /// Pulse — runtime-vitality reading: RPM tachometer, six-voyant strip (ADR-138 P1)
    #[command(after_help = cmd::examples::PULSE)]
    Pulse(cmd::pulse::Args),

    /// Project — materialize views from the ledger (STATUS.md, ISSUES.md, GitHub Issues)
    #[command(after_help = cmd::examples::PROJECT)]
    Project(cmd::reconcile::Args),

    /// Reconcile — **deprecated** alias for `cs project` (ADR-052 §D3)
    #[command(after_help = cmd::examples::RECONCILE)]
    Reconcile(cmd::reconcile::Args),

    /// Scheduler — read-only view onto `cosmon-scheduler`'s state (patrols, last fires, log)
    #[command(after_help = cmd::examples::SCHEDULER)]
    Scheduler(cmd::scheduler::Args),

    /// Security — operator-only binary posture toggle (prepared ↔ active)
    #[command(after_help = cmd::examples::SECURITY, hide = true)]
    Security(cmd::security::Args),

    /// Sensorium — read the five-organ aggregate behind `cs peek --snapshot`'s vital strip
    #[command(hide = true)]
    Sensorium(cmd::sensorium::Args),

    /// Session — operator carnet (start/note/end), append-only, BLAKE3-sealed
    #[command(after_help = cmd::examples::SESSION)]
    Session(cmd::session::Args),

    /// Daemons — operator view over `cosmon-daemon-supervisor` (list/status/reload/logs)
    #[command(after_help = cmd::examples::DAEMONS)]
    Daemons(cmd::daemons::Args),

    /// Project pulse — quick DAG overview like git status
    #[command(after_help = cmd::examples::STATUS)]
    Status(cmd::status::Args),

    /// Tackle a molecule — spawn ONE worker on this node (always leaf; for DAG walks use `cs run`)
    #[command(after_help = cmd::examples::TACKLE)]
    Tackle(cmd::tackle::Args),

    /// Execute a detached Ollama-backed local worker (internal transport command).
    #[command(hide = true)]
    LocalWorker(cmd::tackle::LocalWorkerArgs),

    /// Claim a molecule for the pilot; the runtime defers until it is released
    #[command(after_help = cmd::examples::CLAIM)]
    Claim(cmd::claim::Args),

    /// Release a pilot claim and return the molecule to the runtime frontier
    #[command(after_help = cmd::examples::RELEASE)]
    Release(cmd::claim::Args),

    /// Tag — add or remove typed labels on a molecule
    #[command(after_help = cmd::examples::TAG)]
    Tag(cmd::tag::Args),

    /// Tail — live `notify`-driven reader over `events.jsonl` (fleet or `--all-galaxies`)
    #[command(after_help = cmd::examples::TAIL)]
    Tail(cmd::tail::Args),

    /// Tokens — read-only aggregator over the token-meter NDJSON sink (per-worker token usage from the recorded event trail)
    #[command(hide = true)]
    Tokens(cmd::tokens::Args),

    /// Note — append an audit-trail note to a molecule
    #[command(after_help = cmd::examples::NOTE, hide = true)]
    Note(cmd::note::Args),

    /// Paths — project the write-path taxonomy (`--writes`), a pure derived view
    Paths(cmd::paths::Args),

    /// Done — terminal teardown for a molecule (merge + cleanup, human-callable)
    #[command(after_help = cmd::examples::DONE)]
    Done(cmd::done::Args),

    /// Harvest — close a completed-but-unmerged molecule by invoking `cs done`
    #[command(after_help = cmd::examples::HARVEST)]
    Harvest(cmd::harvest::Args),

    /// Stitch — fleet-locked sequential merge of a mission DAG into base (Phase 1 Commit 2)
    #[command(after_help = cmd::examples::STITCH, hide = true)]
    Stitch(cmd::stitch::Args),

    /// Stuck — freeze a molecule and record the blocker
    #[command(after_help = cmd::examples::STUCK)]
    Stuck(cmd::stuck::Args),

    /// Await-operator — the only sanctioned way to block on an operator
    /// decision at an irreversibility boundary (ADR-123). Routes on the
    /// molecule's `op-block:*` capability: block-and-emit, or
    /// surface-and-continue.
    #[command(name = "await-operator", after_help = cmd::examples::AWAIT_OPERATOR)]
    AwaitOperator(cmd::await_operator::Args),

    /// Heartbeat — emit a worker liveness signal (thinking/waiting/idle/unknown)
    #[command(after_help = cmd::examples::HEARTBEAT, hide = true)]
    Heartbeat(cmd::heartbeat::Args),

    /// Topology — structural maps of the workspace (wraps the `topon` CLI)
    #[command(after_help = cmd::examples::TOPOLOGY)]
    Topology(cmd::topology::Args),

    /// Deps — show blocking dependencies for a molecule (upstream/downstream)
    #[command(after_help = cmd::examples::DEPS)]
    Deps(cmd::deps::Args),

    /// Mission — read-only DAG view joining ledger edges to completion merge commits
    Mission(cmd::mission::Args),

    /// Sync — base-sync the current worktree from main, stamping a Base-Sync trailer
    Sync(cmd::sync::Args),

    /// Diverge — structural agreement check between two sessions on a molecule (turing §5)
    #[command(after_help = cmd::examples::DIVERGE)]
    Diverge(cmd::diverge::Args),

    /// Peek — canonical fleet observation command (TUI default; `--no-tui` for plaintext stream)
    #[command(after_help = cmd::examples::PEEK)]
    Peek(cmd::peek::Args),

    /// Wait — block until a molecule reaches a terminal (or requested) status
    #[command(after_help = cmd::examples::WAIT)]
    Wait(cmd::wait::Args),

    /// Internal — first-turn realized-model watcher, spawned detached by
    /// `cs tackle` for session-log adapters (claude/codex). Emits
    /// `ModelObserved` at the first model-bearing assistant turn (D4),
    /// pane-independent so a crashed worker's durable session log still
    /// yields its observation.
    #[command(name = "realized-watch", hide = true)]
    RealizedWatch(cmd::realized_watch::Args),

    /// Run — walk a molecule DAG of N≥1 nodes via the resident runtime (ADR-016 Layer B)
    #[command(after_help = cmd::examples::RUN)]
    Run(cmd::run::Args),

    /// Spore germinates a whole polymer from a shareable `spore.toml` template (validate / run / export, ADR-140)
    #[command(after_help = cmd::examples::SPORE)]
    Spore(cmd::spore::Args),

    /// Show help — all commands grouped, or detailed help for one command
    #[command(after_help = cmd::examples::HELP)]
    Help(cmd::help::Args),

    /// Replay — interactive D3 timeline of the fleet run
    #[command(after_help = cmd::examples::REPLAY, hide = true)]
    Replay(cmd::replay::Args),

    /// Verify — walk a molecule's event hash chain (plumbing v2)
    #[command(after_help = cmd::examples::VERIFY)]
    Verify(cmd::verify::Args),

    /// Validate — deliberate heavyweight project-milestone gate
    Validate(cmd::validate::Args),

    /// `verify-trace` — replay `events.jsonl` against the scheduler spec (Phase 3 CI gate)
    #[command(name = "verify-trace")]
    VerifyTrace(cmd::verify_trace::Args),

    /// `verify-graph` — Tarjan SCC check on the subgraph induced by a typed relation (substrate, ADR-016)
    #[command(name = "verify-graph", after_help = cmd::examples::VERIFY_GRAPH)]
    VerifyGraph(cmd::verify_graph::Args),

    /// `spec-audit` — ledger audit against the TLA+ spec (catches c1cb `bypass_merge`)
    #[command(name = "spec-audit", after_help = cmd::examples::SPEC_AUDIT)]
    SpecAudit(cmd::spec_audit::Args),

    /// `release-audit` — dry-run drift detector for the public distribution (analogue of `reconcile --check`)
    #[command(name = "release-audit", after_help = cmd::examples::RELEASE_AUDIT)]
    ReleaseAudit(cmd::release_audit::Args),

    /// Test — spec-suite entry point (binding audits; NOT a `cargo test` wrapper)
    #[command(after_help = cmd::examples::TEST, hide = true)]
    Test(cmd::test::Args),

    /// Doctor — diagnostic probes (whisper channel, …)
    Doctor(cmd::doctor::Args),

    /// Demo — one-command end-to-end chatbot surface (first-contact experience)
    #[command(after_help = cmd::examples::DEMO)]
    Demo(cmd::demo::Args),

    /// Whisper — inject a perturbation payload into a live worker's tmux pane (v0)
    #[command(after_help = cmd::examples::WHISPER)]
    Whisper(cmd::whisper::Args),

    /// Presence — live-session registry (ping / ls / gc / poll)
    #[command(after_help = cmd::examples::PRESENCE, hide = true)]
    Presence(cmd::presence::Args),

    /// Archive — operator view over durable terminal snapshots (list/show/verify/prune, ADR-030)
    #[command(after_help = cmd::examples::ARCHIVE)]
    Archive(cmd::archive::Args),

    /// Opt-in-share — first-run consent prompt for encrypted developer bundles
    #[command(name = "opt-in-share", after_help = cmd::examples::OPT_IN_SHARE)]
    OptInShare(cmd::opt_in_share::Args),

    /// Notarize — issue or verify an operator-signed attestation for a molecule (ADR-056)
    Notarize(cmd::notarize::Args),

    /// Panel — convene a hash-pinned supermajority panel to gate a constitutional amendment
    #[command(after_help = cmd::examples::PANEL)]
    Panel(cmd::panel::Args),

    /// Witness — Layer-2 witness-quorum seal for stress-test molecules (ADR-085 §3)
    #[command(after_help = cmd::examples::WITNESS)]
    Witness(cmd::witness::Args),

    /// Notify — push a one-line message to every configured operator channel
    #[command(after_help = cmd::examples::NOTIFY)]
    Notify(cmd::notify::Args),

    /// Key — manage the operator's Ed25519 notary key (generate, show)
    #[command(after_help = cmd::examples::KEY)]
    Key(cmd::key::Args),

    /// Inspect — classify a path into its genre (ADR-057)
    #[command(after_help = cmd::examples::INSPECT, hide = true)]
    Inspect(cmd::inspect::Args),

    /// Artifacts — operator view over the artifact map (audit subcommand, ADR-057)
    #[command(after_help = cmd::examples::ARTIFACTS, hide = true)]
    Artifacts(cmd::artifacts::Args),

    /// Cluster — read/edit/bootstrap the machine-level cluster topology (ADR-066)
    #[command(after_help = cmd::examples::CLUSTER, hide = true)]
    Cluster(cmd::cluster::Args),

    /// Apps — operator view over the cluster's HTTP-on-Tailscale daemons
    #[command(hide = true)]
    Apps(cmd::apps::Args),

    /// Config — inspect `.cosmon/config.toml` (`show adapters`, `adapters`)
    Config(cmd::config::Args),

    /// vllm-mlx — pre-flight + diagnostics for the local-inference sidecar
    #[command(name = "vllm-mlx", hide = true)]
    VllmMlx(cmd::vllm_mlx::Args),

    /// Introspection — print a newline-separated list of every command
    /// path in the clap tree (e.g. `migrate`, `migrate to`, …). Hidden
    /// plumbing used by the `help_goldens` integration test to walk the
    /// full surface without a hand-written list.
    #[command(name = "__help-tree", hide = true)]
    HelpTree {
        /// Include `hide = true` subcommands too. Default output (no
        /// `--all`) lists only visible verbs — that is what the
        /// `help_goldens` walker snapshots. `--all` is for the
        /// UX-CLI parity audit, which tracks every real verb (hidden
        /// or not) since API exposure is orthogonal to book visibility.
        #[arg(long)]
        all: bool,
    },

    /// Introspection — render the man page from the live clap tree and
    /// emit it on stdout. Hidden plumbing used by the `help_goldens`
    /// integration test to catch drift between the derive tree and the
    /// committed `man/cs.1`.
    #[command(name = "__man-page", hide = true)]
    ManPage,

    /// Introspection — render the mdBook Reference pages
    /// (`docs/book/src/reference/*.md`) from the live clap tree, one file
    /// per `command_group_layout()` group. Hidden plumbing: the markdown
    /// twin of `__man-page`. `--out <DIR>` writes the pages; with no
    /// `--out` the manifest of generated filenames is printed. The
    /// `help_goldens` integration test regenerates into a temp dir and
    /// diffs against the committed pages so the Reference cannot drift
    /// from the CLI signature surface (ADR-B1′ §5.5).
    #[command(name = "__markdown-help", hide = true)]
    MarkdownHelp(cmd::markdown_help::Args),

    /// Introspection — print the git commit SHA this binary was built
    /// from (the `COSMON_BUILD_SHA` stamp). Hidden plumbing used by the
    /// `cs done` deploy-verification step to assert the freshly-installed
    /// binary matches the just-merged HEAD. Prints
    /// the full SHA (or `unknown`) on stdout, one line, no newline noise.
    #[command(name = "__build-sha", hide = true)]
    BuildSha,
}

#[allow(clippy::too_many_lines)]
fn main() {
    let cli = Cli::parse();

    let ctx = cmd::Context {
        verbose: cli.verbose,
        json: cli.json,
        config: cli.config,
    };

    // STREAM half of layer B compromise (delib-20260509-18df §D-B,
    // task-20260509-9f78). Emit `operator.present` once per
    // interactive invocation so the `operator-attention-patrol` proxy
    // has a foundational signal — last-touch timestamp per session.
    // Best-effort: no-op when the state dir does not exist yet, and
    // any error is swallowed so the hot path proceeds.
    {
        let state_dir = cosmon_filestore::resolve_state_dir(None);
        let sid = operator_event::current_session_id();
        let nucleon_id = operator_event::current_nucleon_id();
        let orbitale_id = operator_event::current_orbitale_id();
        operator_event::emit_operator_present(
            &state_dir,
            &sid,
            nucleon_id.as_deref(),
            orbitale_id.as_deref(),
            // V0: Internal — cosmon writes the substrate, no-cloning
            // theorem prevents downstream destructive-action gating
            // from trusting this. A follow-up molecule wires the
            // exogenous IoregSensor poll into this emission point.
            cosmon_core::presence_sensor::PresenceSource::Internal,
        );

        // OperatorSigned — record destructive verbs *before* dispatch
        // so the trace captures the gesture even if the action errors
        // out. V0 records the gesture; gating is deferred until the
        // 2-week Mach test has run.
        let signed_action = match &cli.command {
            Command::Done(_) => Some("cs done"),
            Command::Collapse(_) => Some("cs collapse"),
            Command::Purge(_) => Some("cs purge"),
            Command::Kill(_) => Some("cs kill"),
            Command::Teardown(_) => Some("cs teardown"),
            Command::Stuck(_) => Some("cs stuck"),
            _ => None,
        };
        if let Some(action) = signed_action {
            operator_event::emit_operator_signed(&state_dir, action, None, "shell");
        }
    }

    let result = match cli.command {
        Command::Ensemble(args) => cmd::ensemble::run(&ctx, &args),
        Command::Nucleate(args) => cmd::nucleate::run(&ctx, &args),
        Command::Observe(args) => cmd::observe::run(&ctx, &args),
        Command::Events(args) => cmd::events::run(&ctx, &args),
        Command::Errors(args) => cmd::errors::run(&ctx, &args),
        Command::Evolve(args) => cmd::evolve::run(&ctx, &args),
        Command::Collapse(args) => cmd::collapse::run(&ctx, &args),
        Command::Complete(args) => cmd::complete::run(&ctx, &args),
        Command::Decay(args) => cmd::interaction::run_decay(&ctx, &args),
        Command::Merge(args) => cmd::interaction::run_merge(&ctx, &args),
        Command::Transform(args) => cmd::interaction::run_transform(&ctx, &args),
        Command::Init(args) => cmd::init::run(&ctx, &args),
        Command::Inbox(args) => cmd::inbox::run(&ctx, &args),
        Command::Spark(args) => cmd::spark::run(&ctx, &args),
        Command::Drop(args) => cmd::drop::run(&ctx, &args),
        Command::Listen(args) => cmd::listen::run(&ctx, &args),
        Command::Ask(args) => cmd::ask::run(&ctx, &args),
        Command::Pilot(args) => cmd::pilot::run(&ctx, &args),
        Command::Kill(args) => cmd::kill::run(&ctx, &args),
        Command::Quench(args) => cmd::quench::run(&ctx, &args),
        Command::Fleet(args) => cmd::fleet::run(&ctx, &args),
        Command::Galaxies(args) => cmd::galaxies::run(&ctx, &args),
        Command::Mur(args) => cmd::mur::run(&ctx, &args),
        Command::Motion(args) => cmd::motion::run(&ctx, &args),
        Command::Freeze(args) => cmd::freeze::run(&ctx, &args),
        Command::Thaw(args) => cmd::thaw::run(&ctx, &args),
        Command::Trust(args) => cmd::trust::run(&ctx, &args),
        Command::Resume(args) => cmd::resume::run(&ctx, &args),
        Command::Resurrect(args) => cmd::resurrect::run(&ctx, &args),
        Command::Teardown(args) => cmd::teardown::run(&ctx, &args),
        Command::Purge(args) => cmd::purge::run(&ctx, &args),
        Command::Prime(args) => cmd::prime::run(&ctx, &args),
        Command::Migrate(args) => cmd::migrate::run(&ctx, &args),
        Command::Patrol(args) => cmd::patrol::run(&ctx, &args),
        Command::Health(args) => cmd::health::run(&ctx, &args),
        Command::Pulse(args) => cmd::pulse::run(&ctx, &args),
        Command::Project(args) => cmd::reconcile::run(&ctx, &args),
        Command::Reconcile(args) => cmd::reconcile::run_reconcile_alias(&ctx, &args),
        Command::Scheduler(args) => cmd::scheduler::run(&ctx, &args),
        Command::Security(args) => cmd::security::run(&ctx, &args),
        Command::Sensorium(args) => cmd::sensorium::run(&ctx, &args),
        Command::Session(args) => cmd::session::run(&ctx, &args),
        Command::Daemons(args) => cmd::daemons::run(&ctx, &args),
        Command::Status(args) => cmd::status::run(&ctx, &args),
        Command::Tackle(args) => cmd::tackle::run(&ctx, &args),
        Command::LocalWorker(args) => cmd::tackle::run_local_worker(&args),
        Command::Claim(args) => cmd::claim::claim(&ctx, &args),
        Command::Release(args) => cmd::claim::release(&ctx, &args),
        Command::Tag(args) => cmd::tag::run(&ctx, &args),
        Command::Tail(args) => cmd::tail::run(&ctx, &args),
        Command::Tokens(args) => cmd::tokens::run(&ctx, &args),
        Command::Note(args) => cmd::note::run(&ctx, &args),
        Command::Paths(args) => cmd::paths::run(&ctx, &args),
        Command::Done(args) => cmd::done::run(&ctx, &args),
        Command::Harvest(args) => cmd::harvest::run(&ctx, &args),
        Command::Stitch(args) => cmd::stitch::run(&ctx, &args),
        Command::Stuck(args) => cmd::stuck::run(&ctx, &args),
        Command::AwaitOperator(args) => cmd::await_operator::run(&ctx, &args),
        Command::Heartbeat(args) => cmd::heartbeat::run(&ctx, &args),
        Command::Topology(args) => cmd::topology::run(&ctx, &args),
        Command::Deps(args) => cmd::deps::run(&ctx, &args),
        Command::Mission(args) => cmd::mission::run(&ctx, &args),
        Command::Sync(args) => cmd::sync::run(&ctx, &args),
        Command::Diverge(args) => cmd::diverge::run(&ctx, &args),
        Command::Peek(args) => cmd::peek::run(&ctx, &args),
        Command::Wait(args) => cmd::wait::run(&ctx, &args),
        Command::RealizedWatch(args) => cmd::realized_watch::run(&ctx, &args),
        Command::Run(args) => cmd::run::run(&ctx, &args),
        Command::Spore(args) => cmd::spore::run(&ctx, &args),
        Command::Help(args) => cmd::help::run(&args),
        Command::Replay(args) => cmd::replay::run(&ctx, &args),
        Command::Verify(args) => cmd::verify::run(&ctx, &args),
        Command::Validate(args) => cmd::validate::run(&ctx, &args),
        Command::VerifyTrace(args) => cmd::verify_trace::run(&ctx, &args),
        Command::VerifyGraph(args) => cmd::verify_graph::run(&ctx, &args),
        Command::SpecAudit(args) => cmd::spec_audit::run(&ctx, &args),
        Command::ReleaseAudit(args) => cmd::release_audit::run(&ctx, &args),
        Command::Test(args) => cmd::test::run(&ctx, &args),
        Command::Doctor(args) => cmd::doctor::run(&ctx, &args),
        Command::Whisper(args) => cmd::whisper::run(&ctx, &args),
        Command::Presence(args) => cmd::presence::run(&ctx, &args),
        Command::Demo(args) => cmd::demo::run(&ctx, &args),
        Command::Archive(args) => cmd::archive::run(&ctx, &args),
        Command::OptInShare(args) => cmd::opt_in_share::run(&ctx, &args),
        Command::Notarize(args) => cmd::notarize::run(&ctx, &args),
        Command::Panel(args) => cmd::panel::run(&ctx, &args),
        Command::Witness(args) => cmd::witness::run(&ctx, &args),
        Command::Notify(args) => cmd::notify::run(&ctx, &args),
        Command::Key(args) => cmd::key::run(&ctx, &args),
        Command::Inspect(args) => cmd::inspect::run(&ctx, &args),
        Command::Artifacts(args) => cmd::artifacts::run(&ctx, &args),
        Command::Cluster(args) => cmd::cluster::run(&ctx, &args),
        Command::Apps(args) => cmd::apps::run(&ctx, &args),
        Command::Config(args) => cmd::config::run(&ctx, &args),
        Command::VllmMlx(args) => cmd::vllm_mlx::run(&ctx, &args),
        Command::HelpTree { all } => {
            print_help_tree(all);
            Ok(())
        }
        Command::ManPage => print_man_page(),
        Command::MarkdownHelp(args) => cmd::markdown_help::run(&build_cli(), &args),
        Command::BuildSha => {
            println!("{}", cosmon_cli::BUILD_SHA);
            Ok(())
        }
    };

    if let Err(e) = result {
        if ctx.json {
            let err = serde_json::json!({"error": format!("{e:#}")});
            eprintln!("{err}");
        } else {
            eprintln!("cs: {e:#}");
        }
        // Typed CLI guard refusals carry their own exit codes so
        // scripts can branch on the specific rule that fired. Any
        // other error falls through to the generic exit-1 path.
        if let Some(code) = cmd::session::extract_exit_code(&e) {
            std::process::exit(code);
        }
        let code: i32 = e
            .downcast_ref::<cmd::guard::GuardError>()
            .map_or(1, cmd::guard::GuardError::exit_code);
        std::process::exit(code);
    }
}

/// Expose the fully-assembled clap tree for introspection and tests.
///
/// The derive macros on [`Cli`] produce the canonical tree; everything
/// else (the help renderer, the committed man page, the goldens) reads
/// from here. This is the "one source, two readers" spine for the CLI
/// documentation surface.
#[must_use]
pub fn build_cli() -> clap::Command {
    <Cli as CommandFactory>::command()
}

/// Implementation of the hidden `__help-tree` subcommand.
///
/// Emits one line per (sub)command path, breadth-first, so integration
/// tests can iterate the whole help surface without a hand-written
/// list. The root command is emitted as a blank line so the caller
/// sees the full depth structure uniformly.
fn print_help_tree(all: bool) {
    fn walk(parent_path: &[&str], cmd: &clap::Command, all: bool, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            // The `__*` plumbing verbs are never emitted (they are not
            // part of the documented surface at all); `--all` only lifts
            // the visibility filter on ordinary hidden verbs.
            if sub.get_name().starts_with("__") {
                continue;
            }
            if sub.is_hide_set() && !all {
                continue;
            }
            let name = sub.get_name();
            let mut path: Vec<&str> = parent_path.to_vec();
            path.push(name);
            out.push(path.join(" "));
            walk(&path, sub, all, out);
        }
    }

    let root = build_cli();
    let mut paths = Vec::new();
    walk(&[], &root, all, &mut paths);
    for p in paths {
        println!("{p}");
    }
}

/// Implementation of the hidden `__man-page` subcommand.
///
/// Renders the man page from the live clap tree and writes it to
/// stdout. The [`help_goldens`](../tests/help_goldens.rs) integration
/// test compares this output to the committed `man/cs.1` so any drift
/// between the derive tree and the on-disk man page fails CI.
fn print_man_page() -> anyhow::Result<()> {
    use std::io::Write;

    // The committed man page is a golden artifact: rendering it with
    // the full build stamp (SHA, dirty, date) would make it drift on
    // every commit. Pin the documented surface to the plain crate
    // version; the stamp belongs to `--version` only.
    let cmd = build_cli().version(env!("CARGO_PKG_VERSION"));
    let man = clap_mangen::Man::new(cmd);
    let mut buf: Vec<u8> = Vec::new();
    man.render(&mut buf)
        .map_err(|e| anyhow::anyhow!("render man page: {e}"))?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(&buf)
        .map_err(|e| anyhow::anyhow!("write man page: {e}"))?;
    Ok(())
}
