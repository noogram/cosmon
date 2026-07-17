// SPDX-License-Identifier: Apache-2.0

//! `cs-thin` command dispatch — clap parser, exit-code mapping, and HTTP
//! plumbing for the three V0 verbs (observe / nucleate / tag).
//!
//! Lives in the library (not the binary) so integration tests can drive
//! the dispatcher in-process: the test starts the rpp-adapter on a local
//! `TcpListener`, mints a JWT, and calls [`run_with`] with a
//! `Stdout`-shaped `Vec<u8>` to capture the bytes the operator would see.
//!
//! # Exit-code contract
//!
//! - `0` — success.
//! - `1` — backend error (HTTP 4xx/5xx, molecule not found, validation).
//! - `2` — network error (DNS, TCP, TLS) — same shape as `curl`'s 7.
//! - `3` — JWT configuration error (no env var, no file, both empty).
//!
//! # Output discipline
//!
//! The renderer for each verb projects the rpp-adapter response into
//! the byte-stable JSON shape produced by `cs --json <verb>` — that is
//! the §8p invariant we have to preserve. `observe` returns the
//! `molecule` envelope verbatim; `nucleate` projects to the
//! `NucleateJson` shape (`id`, `formula`, `status`, `total_steps`,
//! `assigned_worker`, `variables`, `created_at`); `tag` returns the
//! `tag` envelope verbatim. The rpp-adapter response is the source of
//! truth — cs-thin re-renders rather than re-computing.

use clap::{Parser, Subcommand};
use serde_json::{json, Map, Value};

use crate::coverage::{build_report, render_human, render_json};

/// Top-level CLI for `cs-thin`.
///
/// Configuration is split between flags and environment for the JWT —
/// `--jwt-from-env <NAME>` reads the bearer from the named environment
/// variable (default `JWT`), `--jwt-file <PATH>` reads it from disk,
/// and the two are mutually exclusive. We do not persist credentials
/// by default: the discipline is to inject the token at process start
/// and let it die with the process.
#[derive(Debug, Parser)]
#[command(
    name = "cs-thin",
    version,
    about = "Mechanical HTTP client for the §8p RPP-exposable cosmon verb subset.",
    long_about = None,
    // We ship our own `help` subcommand (renders the structured
    // operator-facing reference). Disable clap's built-in `help`
    // subcommand to avoid the duplicate-name panic.
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Base URL of the cosmon RPP endpoint, schemed (`https://...` in
    /// production, `http://...` for local smoke tests). Trailing
    /// slashes are trimmed.
    ///
    /// May also be supplied via the `CS_THIN_BASE_URL` environment
    /// variable; the flag wins when both are present. Resolution
    /// happens in [`run_with`] (we do it by hand instead of with
    /// clap's `env` feature so the workspace `clap` dependency stays
    /// minimal).
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,

    /// Read the bearer JWT from the named environment variable
    /// (default: `JWT`). Mutually exclusive with `--jwt-file`.
    #[arg(long, value_name = "ENV_VAR")]
    pub jwt_from_env: Option<String>,

    /// Read the bearer JWT from a file on disk (single-line ASCII).
    /// Mutually exclusive with `--jwt-from-env`.
    #[arg(long, value_name = "PATH", conflicts_with = "jwt_from_env")]
    pub jwt_file: Option<std::path::PathBuf>,

    /// Emit a structured coverage report (machine-readable JSON when
    /// `--json` is also set, otherwise the same human form as
    /// `verbs --check`). Top-level so the CI gate can run
    /// `cs-thin --coverage-report --json` without picking a
    /// subcommand. When set, the subcommand becomes optional and is
    /// ignored.
    #[arg(long)]
    pub coverage_report: bool,

    /// Force JSON output. For dispatch verbs cs-thin is JSON-native, so
    /// the flag is a **silent no-op** — accepted for muscle-memory
    /// parity with `cs --json <verb>` (the operator typed it on
    /// 2026-05-05 and got a raw clap error; pinned by
    /// `operator_ux::habit_flag_json_is_silent_noop_on_top_level`).
    /// When paired with `--coverage-report`, selects compact JSON
    /// over the human render.
    #[arg(long)]
    pub json: bool,

    /// Subcommand to execute. Optional when `--coverage-report` is
    /// set; required otherwise.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Sub-commands wired in T-CST-V0 / T-CST-EXPAND.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// `GET /v1/molecules/:id` — read a molecule's state.
    Observe(ObserveArgs),

    /// `POST /v1/molecules` — create a new molecule from a formula.
    Nucleate(NucleateArgs),

    /// `POST /v1/molecules/:id/tags` — add or remove typed labels.
    Tag(TagArgs),

    /// `GET /v1/molecules` — list molecules (filtered).
    Ensemble(EnsembleArgs),

    /// `POST /v1/molecules/:id/collapse` — terminal transition with reason.
    Collapse(CollapseArgs),

    /// `POST /v1/molecules/:id/freeze` — Running → Frozen.
    Freeze(FreezeArgs),

    /// `POST /v1/molecules/:id/thaw` — Frozen → Running.
    Thaw(ThawArgs),

    /// `POST /v1/molecules/:id/stuck` — record a blocker and freeze.
    Stuck(StuckArgs),

    /// `POST /v1/molecules/:id/tackle` — spawn a worker session
    /// (remote-tackle V2).
    Tackle(TackleArgs),

    /// D-AVATAR instance lifecycle.
    Avatar(AvatarArgs),

    /// Inspect the registered verbs (link-time slice).
    Verbs(VerbsArgs),

    /// Show help — overview of cs-thin, or detailed help for one verb.
    Help(HelpArgs),
}

/// Arguments for `cs-thin observe`.
#[derive(Debug, clap::Args)]
pub struct ObserveArgs {
    /// Molecule id (full form, no prefix resolution).
    pub molecule_id: String,
}

/// Arguments for `cs-thin nucleate`.
#[derive(Debug, clap::Args)]
pub struct NucleateArgs {
    /// Formula name. Resolved server-side under
    /// `<galaxies_root>/<noyau>/.cosmon/formulas/<formula>.formula.toml`.
    #[arg(long)]
    pub formula: String,

    /// Optional molecule kind (`task`, `idea`, `decision`, …).
    #[arg(long)]
    pub kind: Option<String>,

    /// Variable binding `key=value`. Repeatable.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    pub vars: Vec<String>,

    /// Tag to add at nucleation. Repeatable.
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
}

/// Arguments for `cs-thin tag`.
#[derive(Debug, clap::Args)]
pub struct TagArgs {
    /// Molecule id (full form).
    pub molecule_id: String,

    /// Tag to add. Repeatable.
    #[arg(long = "add", value_name = "TAG")]
    pub add: Vec<String>,

    /// Tag to remove. Repeatable.
    #[arg(long = "remove", value_name = "TAG")]
    pub remove: Vec<String>,
}

/// Arguments for `cs-thin ensemble` (T-CST-EXPAND).
#[derive(Debug, clap::Args)]
pub struct EnsembleArgs {
    /// Filter by status (`pending`, `running`, `frozen`, …).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by molecule kind (`task`, `idea`, …).
    #[arg(long)]
    pub kind: Option<String>,
    /// Tag glob pattern. Repeatable.
    #[arg(long = "tag", value_name = "GLOB")]
    pub tag: Vec<String>,
    /// Optional fleet filter.
    #[arg(long)]
    pub fleet: Option<String>,
}

/// Arguments for `cs-thin collapse` (T-CST-EXPAND).
#[derive(Debug, clap::Args)]
pub struct CollapseArgs {
    /// Molecule id.
    pub molecule_id: String,
    /// Reason for the collapse (mandatory).
    #[arg(long)]
    pub reason: String,
    /// Structured cause attribution.
    #[arg(long)]
    pub cause: Option<String>,
    /// Account alias (only with `--cause rate_limit`).
    #[arg(long)]
    pub account: Option<String>,
    /// Quota currency name (only with `--cause rate_limit`).
    #[arg(long)]
    pub kind: Option<String>,
}

/// Arguments for `cs-thin freeze` (T-CST-EXPAND).
#[derive(Debug, clap::Args)]
pub struct FreezeArgs {
    /// Molecule id.
    pub molecule_id: String,
    /// Optional reason.
    #[arg(long)]
    pub reason: Option<String>,
}

/// Arguments for `cs-thin thaw` (T-CST-EXPAND).
#[derive(Debug, clap::Args)]
pub struct ThawArgs {
    /// Molecule id.
    pub molecule_id: String,
}

/// Arguments for `cs-thin stuck` (T-CST-EXPAND).
#[derive(Debug, clap::Args)]
pub struct StuckArgs {
    /// Molecule id.
    pub molecule_id: String,
    /// Mandatory blocker reason.
    #[arg(long)]
    pub reason: String,
}

/// Arguments for `cs-thin tackle` (remote-tackle V2).
#[derive(Debug, clap::Args)]
pub struct TackleArgs {
    /// Molecule id (full form, no prefix resolution).
    pub molecule_id: String,
}

/// Arguments for `cs-thin avatar`.
#[derive(Debug, clap::Args)]
pub struct AvatarArgs {
    #[command(subcommand)]
    pub sub: AvatarSub,
}

/// Avatar sub-verbs — instance lifecycle (§8p D-AVATAR).
#[derive(Debug, Subcommand)]
pub enum AvatarSub {
    /// `GET /v1/avatar/:instance_id/status` — mould or avatar state.
    Status(AvatarStatusArgs),
    /// `POST /v1/avatar/:instance_id/incarnate` — bind moule→avatar.
    Incarnate(AvatarIncarnateArgs),
    /// `POST /v1/avatar/:instance_id/grant` — bind a canal.
    Grant(AvatarGrantArgs),
    /// `GET /v1/avatar/:instance_id/audit` — cicatrice + events.
    Audit(AvatarAuditArgs),
    /// `GET /v1/avatar/:instance_id/mould-info` — pre-incarnation info.
    MouldInfo(AvatarMouldInfoArgs),
}

/// Arguments for `cs-thin avatar status`.
#[derive(Debug, clap::Args)]
pub struct AvatarStatusArgs {
    /// Instance identifier.
    pub instance_id: String,
}

/// Arguments for `cs-thin avatar incarnate`.
#[derive(Debug, clap::Args)]
pub struct AvatarIncarnateArgs {
    /// Instance identifier.
    pub instance_id: String,
    /// Pilote DID (e.g. `did:key:z6Mk...`).
    #[arg(long)]
    pub pilote: String,
    /// Tenant identifier.
    #[arg(long)]
    pub tenant: String,
    /// ISO 3166-1 alpha-2 jurisdiction (e.g. `FR`).
    #[arg(long)]
    pub juridiction: String,
}

/// Arguments for `cs-thin avatar grant`.
#[derive(Debug, clap::Args)]
pub struct AvatarGrantArgs {
    /// Instance identifier.
    pub instance_id: String,
    /// Canal to grant (`b`, `c`, or `d`).
    #[arg(long)]
    pub canal: String,
    /// Target identity to bind the canal to.
    #[arg(long)]
    pub target: String,
}

/// Arguments for `cs-thin avatar audit`.
#[derive(Debug, clap::Args)]
pub struct AvatarAuditArgs {
    /// Instance identifier.
    pub instance_id: String,
}

/// Arguments for `cs-thin avatar mould-info`.
#[derive(Debug, clap::Args)]
pub struct AvatarMouldInfoArgs {
    /// Instance identifier.
    pub instance_id: String,
}

/// Arguments for `cs-thin verbs`.
#[derive(Debug, clap::Args)]
pub struct VerbsArgs {
    /// Render the §8p RPP-exposable coverage report — one line per
    /// covered verb (✓), one line per operator-only verb (⚠), then a
    /// summary block citing ADR-080 §5.1. This is the line a operator-demo
    /// would screenshot for the audit doc.
    #[arg(long)]
    pub check: bool,

    /// Emit the report as JSON instead of human-readable text. Only
    /// meaningful in combination with `--check`. Equivalent to the
    /// top-level `cs-thin --coverage-report --json`.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `cs-thin help`.
#[derive(Debug, clap::Args)]
pub struct HelpArgs {
    /// Verb to show detailed help for (delegates to
    /// `cs-thin <verb> --help`). Without an argument, prints the
    /// grouped reference rendered by [`crate::help::render_root_help`].
    pub command: Option<String>,
}

/// Resolve the bearer JWT from either `--jwt-from-env` (default `JWT`)
/// or `--jwt-file`. Empty values count as "absent" and produce
/// [`CliError::JwtMissing`].
fn resolve_jwt(cli: &Cli) -> Result<String, CliError> {
    if let Some(path) = &cli.jwt_file {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CliError::JwtMissing(format!("--jwt-file `{}`: {e}", path.display())))?;
        let trimmed = raw.trim().to_owned();
        if trimmed.is_empty() {
            return Err(CliError::JwtMissing(format!(
                "--jwt-file `{}` is empty",
                path.display()
            )));
        }
        return Ok(trimmed);
    }
    let env_var = cli.jwt_from_env.as_deref().unwrap_or("JWT");
    let raw = std::env::var(env_var).map_err(|_| {
        CliError::JwtMissing(format!(
            "JWT required (set {env_var} env var or pass --jwt-file)"
        ))
    })?;
    let trimmed = raw.trim().to_owned();
    if trimmed.is_empty() {
        return Err(CliError::JwtMissing(format!(
            "JWT required ({env_var} env var is empty)"
        )));
    }
    Ok(trimmed)
}

/// Errors surfaced by `run` / [`run_with`].
///
/// The variants map one-to-one onto the documented exit codes (see
/// [`CliError::exit_code`]). The wire-shape callers rely on those
/// codes — `cs-thin observe nonexistent` exits 1, a network failure
/// exits 2, a missing JWT exits 3.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// JWT missing or empty — exit 3.
    #[error("{0}")]
    JwtMissing(String),

    /// Network failure — DNS, TCP, TLS — exit 2.
    #[error("Network error: {0}")]
    Network(String),

    /// Non-2xx HTTP response — exit 1.
    #[error("Server returned {status}: {body}")]
    Http {
        /// HTTP status code returned by the server.
        status: u16,
        /// Best-effort body excerpt (truncated to 1 KiB).
        body: String,
    },

    /// Local validation, JSON decode, or argument parse failure — exit 1.
    #[error("{0}")]
    Local(String),
}

impl CliError {
    /// Exit code for this error variant. See module docs.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::JwtMissing(_) => 3,
            Self::Network(_) => 2,
            Self::Http { .. } | Self::Local(_) => 1,
        }
    }
}

/// Library-level entry point: run the dispatcher and write the bytes
/// the operator would see to `out`. Returns the resolved exit code.
///
/// The integration test calls this with a `Vec<u8>` to capture the
/// JSON without having to spawn a child process. Errors land on
/// `stderr` — the test does not assert on stderr today, only on the
/// JSON shape.
///
/// # Errors
///
/// See [`CliError`] for the mapping. Top-level callers should call
/// [`CliError::exit_code`] and `std::process::exit` with the result.
pub async fn run_with<W: std::io::Write>(cli: Cli, out: &mut W) -> Result<(), CliError> {
    // Top-level `--coverage-report` short-circuits the subcommand.
    // We resolve the optional `target` from the same precedence chain
    // as the other commands (flag wins over env var); resolution
    // failure is *not* fatal — the report is a self-description, not
    // a probe, so it works offline.
    if cli.coverage_report {
        return run_coverage_report(&cli, cli.json, out);
    }
    let Some(command) = &cli.command else {
        return Err(CliError::Local(
            "missing subcommand (run `cs-thin help` or `cs-thin --coverage-report`)".into(),
        ));
    };
    match command {
        Command::Observe(args) => run_observe(&cli, args, out).await,
        Command::Nucleate(args) => run_nucleate(&cli, args, out).await,
        Command::Tag(args) => run_tag(&cli, args, out).await,
        Command::Ensemble(args) => run_ensemble(&cli, args, out).await,
        Command::Collapse(args) => run_collapse(&cli, args, out).await,
        Command::Freeze(args) => run_freeze(&cli, args, out).await,
        Command::Thaw(args) => run_thaw(&cli, args, out).await,
        Command::Stuck(args) => run_stuck(&cli, args, out).await,
        Command::Tackle(args) => run_tackle(&cli, args, out).await,
        Command::Avatar(args) => run_avatar(&cli, args, out).await,
        Command::Verbs(args) => run_verbs(&cli, args, out),
        Command::Help(args) => crate::help::run_help(args, out),
    }
}

/// Resolve the configured base URL without erroring when neither
/// `--base-url` nor `CS_THIN_BASE_URL` is set; the coverage report
/// works offline and just renders `target: (unset)`.
fn optional_target(cli: &Cli) -> Option<String> {
    cli.base_url
        .as_deref()
        .map(|s| s.trim_end_matches('/').to_owned())
        .or_else(|| {
            std::env::var("CS_THIN_BASE_URL")
                .ok()
                .map(|s| s.trim_end_matches('/').to_owned())
        })
}

fn run_coverage_report<W: std::io::Write>(
    cli: &Cli,
    json: bool,
    out: &mut W,
) -> Result<(), CliError> {
    let target = optional_target(cli);
    let report = build_report(target);
    if json {
        let s = render_json(&report).map_err(|e| CliError::Local(format!("encode report: {e}")))?;
        writeln!(out, "{s}").map_err(|e| CliError::Local(e.to_string()))?;
    } else {
        out.write_all(render_human(&report).as_bytes())
            .map_err(|e| CliError::Local(e.to_string()))?;
    }
    Ok(())
}

/// Convenience shim — builds a `reqwest::Client`, resolves the JWT,
/// and trims the trailing slash off the base URL.
fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

fn base_url(cli: &Cli) -> Result<String, CliError> {
    if let Some(b) = &cli.base_url {
        return Ok(b.trim_end_matches('/').to_owned());
    }
    if let Ok(b) = std::env::var("CS_THIN_BASE_URL") {
        return Ok(b.trim_end_matches('/').to_owned());
    }
    Err(CliError::Local(
        "missing --base-url (or CS_THIN_BASE_URL env var)".into(),
    ))
}

async fn run_observe<W: std::io::Write>(
    cli: &Cli,
    args: &ObserveArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!("{}/v1/molecules/{}", base_url(cli)?, args.molecule_id);
    let resp = http_client()
        .get(&url)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;

    let body = read_envelope(resp).await?;
    // Wire-stable: cs --json observe prints exactly the molecule
    // payload, no envelope. The rpp-adapter wraps it under
    // `body.molecule`; we strip that wrapper so the output is
    // byte-identical to the local `cs --json observe`.
    let molecule = body
        .get("molecule")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `molecule` field".into()))?;
    write_json_line(out, &molecule)
}

async fn run_nucleate<W: std::io::Write>(
    cli: &Cli,
    args: &NucleateArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let mut variables = Map::new();
    for kv in &args.vars {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| CliError::Local(format!("--var `{kv}` must be `key=value`")))?;
        variables.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    let mut body = json!({ "formula": args.formula });
    if let Some(kind) = &args.kind {
        body["kind"] = Value::String(kind.clone());
    }
    if !variables.is_empty() {
        body["variables"] = Value::Object(variables);
    }
    if !args.tags.is_empty() {
        body["tags"] = Value::Array(args.tags.iter().cloned().map(Value::String).collect());
    }

    let url = format!("{}/v1/molecules", base_url(cli)?);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;

    let envelope = read_envelope(resp).await?;
    let molecule = envelope
        .get("molecule")
        .ok_or_else(|| CliError::Local("response missing `molecule` field".into()))?;
    let nucleate_view = project_nucleate(molecule)?;
    write_json_line(out, &nucleate_view)
}

async fn run_tag<W: std::io::Write>(
    cli: &Cli,
    args: &TagArgs,
    out: &mut W,
) -> Result<(), CliError> {
    if args.add.is_empty() && args.remove.is_empty() {
        return Err(CliError::Local(
            "nothing to do — supply --add and/or --remove".into(),
        ));
    }
    let jwt = resolve_jwt(cli)?;
    let body = json!({
        "add": args.add,
        "remove": args.remove,
    });

    let url = format!("{}/v1/molecules/{}/tags", base_url(cli)?, args.molecule_id);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;

    let envelope = read_envelope(resp).await?;
    // rpp-adapter wraps TagJson under `tag`; cs --json tag prints the
    // TagJson directly. Strip the wrapper for byte-stable parity.
    let tag = envelope
        .get("tag")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `tag` field".into()))?;
    write_json_line(out, &tag)
}

fn run_verbs<W: std::io::Write>(cli: &Cli, args: &VerbsArgs, out: &mut W) -> Result<(), CliError> {
    if args.check {
        // Delegate to the shared coverage renderer so the human
        // (`verbs --check`) and machine (`--coverage-report --json`)
        // surfaces stay byte-identical for the same compile-time
        // inputs.
        return run_coverage_report(cli, args.json, out);
    }

    // Without `--check`, fall back to the original "list every
    // registered route" affordance — useful when shell-grepping
    // against the link-time slice.
    let mut verbs: Vec<_> = crate::registry::all().iter().collect();
    verbs.sort_by_key(|d| d.name);
    for d in &verbs {
        writeln!(out, "{:<12} {} {}", d.name, d.method, d.path)
            .map_err(|e| CliError::Local(e.to_string()))?;
    }
    Ok(())
}

async fn run_ensemble<W: std::io::Write>(
    cli: &Cli,
    args: &EnsembleArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!("{}/v1/molecules", base_url(cli)?);
    let mut params: Vec<(String, String)> = Vec::new();
    if let Some(s) = &args.status {
        params.push(("status".into(), s.clone()));
    }
    if let Some(k) = &args.kind {
        params.push(("kind".into(), k.clone()));
    }
    for t in &args.tag {
        params.push(("tag".into(), t.clone()));
    }
    if let Some(f) = &args.fleet {
        params.push(("fleet".into(), f.clone()));
    }
    let resp = http_client()
        .get(&url)
        .query(&params)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    // rpp-adapter wraps EnsembleJson under `ensemble`; cs-thin emits the
    // EnsembleJson directly so callers parse the same shape they would
    // get from `cs ensemble --json` (modulo allowlisted divergences).
    let body = envelope
        .get("ensemble")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `ensemble` field".into()))?;
    write_json_line(out, &body)
}

async fn run_collapse<W: std::io::Write>(
    cli: &Cli,
    args: &CollapseArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let mut body = json!({ "reason": args.reason });
    if let Some(c) = &args.cause {
        body["cause"] = Value::String(c.clone());
    }
    if let Some(a) = &args.account {
        body["account"] = Value::String(a.clone());
    }
    if let Some(k) = &args.kind {
        body["kind"] = Value::String(k.clone());
    }
    let url = format!(
        "{}/v1/molecules/{}/collapse",
        base_url(cli)?,
        args.molecule_id
    );
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let collapse = envelope
        .get("collapse")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `collapse` field".into()))?;
    write_json_line(out, &collapse)
}

async fn run_freeze<W: std::io::Write>(
    cli: &Cli,
    args: &FreezeArgs,
    out: &mut W,
) -> Result<(), CliError> {
    // Fusion v1.0.0-rc (`task-20260522-b538`): `state` is mandatory in
    // the new wire shape; `cs-thin freeze` always sends
    // `state: "frozen"`. To resume a molecule, use `cs-thin thaw`
    // (which dispatches to the same endpoint with `state: "active"`).
    let jwt = resolve_jwt(cli)?;
    let mut body = json!({ "state": "frozen" });
    if let Some(r) = &args.reason {
        body["reason"] = Value::String(r.clone());
    }
    let url = format!(
        "{}/v1/molecules/{}/freeze",
        base_url(cli)?,
        args.molecule_id
    );
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let freeze = envelope
        .get("freeze")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `freeze` field".into()))?;
    write_json_line(out, &freeze)
}

async fn run_thaw<W: std::io::Write>(
    cli: &Cli,
    args: &ThawArgs,
    out: &mut W,
) -> Result<(), CliError> {
    // Fusion v1.0.0-rc (`task-20260522-b538`): `cs-thin thaw` forwards
    // to the unified `POST /v1/molecules/:id/freeze` with
    // `{state: "active"}`. The legacy `/thaw` route returns 410 Gone.
    let jwt = resolve_jwt(cli)?;
    let body = json!({ "state": "active" });
    let url = format!(
        "{}/v1/molecules/{}/freeze",
        base_url(cli)?,
        args.molecule_id
    );
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    // Post-fusion the response wrapper is `freeze` regardless of state.
    let freeze = envelope
        .get("freeze")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `freeze` field".into()))?;
    write_json_line(out, &freeze)
}

async fn run_stuck<W: std::io::Write>(
    cli: &Cli,
    args: &StuckArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let body = json!({ "reason": args.reason });
    let url = format!("{}/v1/molecules/{}/stuck", base_url(cli)?, args.molecule_id);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let stuck = envelope
        .get("stuck")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `stuck` field".into()))?;
    write_json_line(out, &stuck)
}

async fn run_tackle<W: std::io::Write>(
    cli: &Cli,
    args: &TackleArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!(
        "{}/v1/molecules/{}/tackle",
        base_url(cli)?,
        args.molecule_id
    );
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let tackle = envelope
        .get("tackle")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `tackle` field".into()))?;
    write_json_line(out, &tackle)
}

async fn run_avatar<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarArgs,
    out: &mut W,
) -> Result<(), CliError> {
    match &args.sub {
        AvatarSub::Status(a) => run_avatar_status(cli, a, out).await,
        AvatarSub::Incarnate(a) => run_avatar_incarnate(cli, a, out).await,
        AvatarSub::Grant(a) => run_avatar_grant(cli, a, out).await,
        AvatarSub::Audit(a) => run_avatar_audit(cli, a, out).await,
        AvatarSub::MouldInfo(a) => run_avatar_mould_info(cli, a, out).await,
    }
}

async fn run_avatar_status<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarStatusArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!("{}/v1/avatar/{}/status", base_url(cli)?, args.instance_id);
    let resp = http_client()
        .get(&url)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let status = envelope
        .get("avatar_status")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `avatar_status` field".into()))?;
    write_json_line(out, &status)
}

async fn run_avatar_incarnate<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarIncarnateArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let body = json!({
        "pilote_id": args.pilote,
        "tenant_id": args.tenant,
        "juridiction": args.juridiction,
    });
    let url = format!(
        "{}/v1/avatar/{}/incarnate",
        base_url(cli)?,
        args.instance_id
    );
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let incarnate = envelope
        .get("incarnate")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `incarnate` field".into()))?;
    write_json_line(out, &incarnate)
}

async fn run_avatar_grant<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarGrantArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let body = json!({
        "canal": args.canal,
        "target": args.target,
    });
    let url = format!("{}/v1/avatar/{}/grant", base_url(cli)?, args.instance_id);
    let resp = http_client()
        .post(&url)
        .bearer_auth(&jwt)
        .json(&body)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let grant = envelope
        .get("grant")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `grant` field".into()))?;
    write_json_line(out, &grant)
}

async fn run_avatar_audit<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarAuditArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!("{}/v1/avatar/{}/audit", base_url(cli)?, args.instance_id);
    let resp = http_client()
        .get(&url)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let audit = envelope
        .get("audit")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `audit` field".into()))?;
    write_json_line(out, &audit)
}

async fn run_avatar_mould_info<W: std::io::Write>(
    cli: &Cli,
    args: &AvatarMouldInfoArgs,
    out: &mut W,
) -> Result<(), CliError> {
    let jwt = resolve_jwt(cli)?;
    let url = format!(
        "{}/v1/avatar/{}/mould-info",
        base_url(cli)?,
        args.instance_id
    );
    let resp = http_client()
        .get(&url)
        .bearer_auth(&jwt)
        .send()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    let envelope = read_envelope(resp).await?;
    let mould_info = envelope
        .get("mould_info")
        .cloned()
        .ok_or_else(|| CliError::Local("response missing `mould_info` field".into()))?;
    write_json_line(out, &mould_info)
}

/// Read a response, mapping non-2xx into [`CliError::Http`].
async fn read_envelope(resp: reqwest::Response) -> Result<Value, CliError> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| CliError::Network(e.to_string()))?;
    if !status.is_success() {
        let mut body = String::from_utf8_lossy(&bytes).to_string();
        if body.len() > 1024 {
            body.truncate(1024);
        }
        return Err(CliError::Http {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_slice(&bytes).map_err(|e| CliError::Local(format!("decode response: {e}")))
}

/// Project a server-side `ObserveJson` payload onto the `cs --json
/// nucleate` shape (`id`, `formula`, `status`, `total_steps`,
/// `assigned_worker`, `variables`, `created_at`).
fn project_nucleate(molecule: &Value) -> Result<Value, CliError> {
    let id = molecule
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Local("nucleate response missing `id`".into()))?;
    let formula = molecule
        .get("formula")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Local("nucleate response missing `formula`".into()))?;
    let total_steps = molecule
        .get("total_steps")
        .and_then(Value::as_u64)
        .ok_or_else(|| CliError::Local("nucleate response missing `total_steps`".into()))?;
    let assigned_worker = molecule.get("worker").cloned().unwrap_or(Value::Null);
    let variables = molecule
        .get("variables")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let created_at = molecule
        .get("created_at")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Local("nucleate response missing `created_at`".into()))?;
    Ok(json!({
        "id": id,
        "formula": formula,
        "status": "active",
        "total_steps": total_steps,
        "assigned_worker": assigned_worker,
        "variables": variables,
        "created_at": created_at,
    }))
}

fn write_json_line<W: std::io::Write>(out: &mut W, value: &Value) -> Result<(), CliError> {
    let s =
        serde_json::to_string(value).map_err(|e| CliError::Local(format!("encode output: {e}")))?;
    writeln!(out, "{s}").map_err(|e| CliError::Local(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_nucleate_emits_canonical_shape() {
        let molecule = json!({
            "id": "task-20260504-aaaa",
            "formula": "task-work",
            "status": "running",
            "total_steps": 3,
            "current_step": 0,
            "worker": "ruby",
            "variables": { "topic": "hello" },
            "created_at": "2026-05-04T12:00:00+00:00",
        });
        let v = project_nucleate(&molecule).unwrap();
        assert_eq!(v["status"], "active");
        assert_eq!(v["formula"], "task-work");
        assert_eq!(v["assigned_worker"], "ruby");
        assert_eq!(v["total_steps"], 3);
        assert_eq!(v["created_at"], "2026-05-04T12:00:00+00:00");
    }

    #[test]
    fn cli_error_exit_codes_are_stable() {
        assert_eq!(CliError::JwtMissing("x".into()).exit_code(), 3);
        assert_eq!(CliError::Network("x".into()).exit_code(), 2);
        assert_eq!(
            CliError::Http {
                status: 404,
                body: "x".into()
            }
            .exit_code(),
            1
        );
        assert_eq!(CliError::Local("x".into()).exit_code(), 1);
    }
}
