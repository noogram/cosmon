// SPDX-License-Identifier: AGPL-3.0-only

//! Clause (e) — subprocess envelope (ADR-080 §3.5).
//!
//! Each admitted request shells out to the real `cs` binary with a
//! non-negotiable envelope. The envelope has **two halves**:
//!
//! - **Set** per-tenant `cwd` and the three correlation env vars
//!   (`COSMON_API_REQUEST=1`, `COSMON_API_REQUEST_ID`,
//!   `COSMON_API_NUCLEON`), plus a hard timeout.
//! - **Clear**, then re-admit only the names on an explicit
//!   allow-list ([`PASSTHROUGH_VARS`]). Nothing of the adapter's own
//!   environment reaches the child unless it is written there with a
//!   reason. This half used to be a deny-list; it was wrong by
//!   default for every variable added elsewhere in the workspace, and
//!   `COSMON_SKIP_PRE_DONE_HOOK` — the operator kill-switch of the
//!   blocking `pre_done` gate — crossed the perimeter through that
//!   gap (delib-20260819-cda2, C2).
//!
//! Stdout/stderr capture is *only* echoed back through response
//! fields the route schema explicitly allows.
//!
//! This module is the only place inside the crate that spawns a child
//! process; tests can exercise admission without touching `cs`.

// clippy 1.89 `similar_names` flags the spawn vars `cmd` / `cwd`; both are
// idiomatic and intentional. File-level allow (toolchain drift).
#![allow(clippy::similar_names)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::admission::Spark;
use crate::error::RppRejectReason;

/// Envelope delivered to every `cs` subprocess. Names match ADR-080
/// §3.5 verbatim — re-naming requires a successor amendment.
pub mod env {
    /// Marks the invocation as RPP-originated; `cs` enforces
    /// operator-only refusal at parse time when this is set.
    pub const COSMON_API_REQUEST: &str = "COSMON_API_REQUEST";
    /// Correlates the audit envelope, child stdout, and the cosmon
    /// event log written by the subprocess.
    pub const COSMON_API_REQUEST_ID: &str = "COSMON_API_REQUEST_ID";
    /// Resolved nucleon for the audit cross-reference.
    pub const COSMON_API_NUCLEON: &str = "COSMON_API_NUCLEON";
    /// Model pin for the worker `claude` session (avatar-surface D1).
    /// Exported when the invoker was configured with
    /// [`super::SystemInvoker::with_claude_model`]; the value comes
    /// from the instance config
    /// ([`crate::config::RppConfig::resolved_claude_model`]) — this
    /// crate never holds a model-id literal outside that config
    /// module. `cs tackle` re-emits the variable across the tmux
    /// boundary into the worker command (`tackle_env::
    /// build_claude_command` in `cosmon-cli`), where the claude CLI
    /// reads it as its model setting.
    pub const ANTHROPIC_MODEL: &str = "ANTHROPIC_MODEL";
}

/// Allow-list half of the §3.5 envelope — the **only** variables of the
/// adapter's own environment that are allowed to cross the
/// adapter→subprocess boundary.
///
/// **Why an allow-list.** This set used to be a deny-list
/// (`STRIP_VARS`): everything was inherited except a hand-maintained
/// roster of `COSMON_*` resolution vars. A security envelope built that
/// way is wrong by default for every variable added anywhere else in
/// the code — the omission is not an oversight, it is the normal
/// behaviour of the structure. The concrete falsification
/// (delib-20260819-cda2, C2) was `COSMON_SKIP_PRE_DONE_HOOK`: the
/// human operator's kill-switch for the blocking `pre_done` gate
/// ([`cosmon_core::config`] `pre_done`), absent from the deny-list, and
/// therefore inherited by the spawned `cs run` drain and by every
/// `cs done` that drain launches. One variable set once — an image
/// entrypoint, a service wrapper, an operations shell — disarmed the
/// Definition-of-Done of every subsequent harvest, inside a container
/// where no operator exists to make the gesture. Same family as the
/// defect `docs/adr/171-the-operator-gesture-is-a-signature-not-a-string.md`
/// falsified: an operator derogation exercisable by someone who is not
/// the operator.
///
/// Under an allow-list the default is reversed: a variable reaches the
/// child only because its name is written here, with a reason. A new
/// `COSMON_*` reader elsewhere in the workspace is closed on arrival,
/// not on the next audit.
///
/// **What is not here.** No `COSMON_*` name at all. The three envelope
/// correlation vars, `COSMON_STATE_DIR`, `COSMON_ARTIFACT_DIR` and the
/// model pin are *set* explicitly by [`SystemInvoker`]'s command
/// builder after the clear, from adapter config — never inherited. Everything
/// else (`COSMON_SKIP_PRE_DONE_HOOK`, `COSMON_OPERATOR_GESTURE`,
/// `COSMON_GALAXY`, `CB_DEPTH`, the pilot vars, …) is simply gone.
///
/// **What is here, and why.** Only process-hygiene variables a POSIX
/// child and the tools `cs` shells out to (git, tmux, sh, claude)
/// cannot work without. Each line is a deliberate hole in the envelope;
/// adding one is a security decision, which is exactly the property the
/// deny-list did not have.
pub const PASSTHROUGH_VARS: &[&str] = &[
    // Process basics. Without `PATH` the child cannot find `git`,
    // `sh` or `tmux`; without `HOME` git and the claude CLI read no
    // configuration at all.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "PWD",
    // Terminal / locale — `cs tackle` drives tmux, and UTF-8 paths
    // round-trip only with a sane locale.
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    // XDG bases: where the child's own tooling keeps per-user state.
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    // Git transport over ssh (agent socket) — a worker that cannot
    // authenticate cannot push a branch.
    "SSH_AUTH_SOCK",
    // Egress proxy configuration, when the deployment has one. Absent
    // from the child, every network call inside the container would
    // bypass the proxy and fail closed at the jail.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // Anthropic credentials / endpoint for the worker `claude` the
    // child launches. `ANTHROPIC_API_KEY` is additionally *set* from
    // config when the invoker was built with
    // [`SystemInvoker::with_anthropic_key`] (the key may live in a
    // file the adapter env never carried); this entry covers the
    // plain-inheritance deployment. `ANTHROPIC_MODEL` is likewise
    // re-set by the pin when one is configured.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    env::ANTHROPIC_MODEL,
    // Diagnostics for the child itself. Read-only knobs; they steer no
    // resolution and grant no capability.
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// Whether `key` is allowed to be inherited by a spawned `cs`.
///
/// The single decision point of the allow-list, exposed so the envelope
/// invariant is testable as a pure predicate rather than only through a
/// spawn. Exact-match by design: no prefix rule, because a prefix rule
/// (`COSMON_*`, `ANTHROPIC_*`) is how a deny-list grows holes again —
/// it admits names nobody has read.
#[must_use]
pub fn is_passthrough(key: &str) -> bool {
    PASSTHROUGH_VARS.contains(&key)
}

/// Result of a successful subprocess invocation.
#[derive(Debug)]
pub struct InvocationResult {
    /// Raw stdout — caller is responsible for parsing.
    pub stdout: Vec<u8>,
    /// Captured stderr (kept for audit logging, not the wire).
    pub stderr: Vec<u8>,
    /// Process exit status.
    pub exit_code: i32,
}

/// Concrete invoker that spawns the `cs` binary as a child process.
#[derive(Clone, Debug)]
pub struct SystemInvoker {
    cs_path: PathBuf,
    galaxies_root: PathBuf,
    timeout: Duration,
    anthropic_api_key: Option<String>,
    /// Root under which per-molecule artifact dirs are materialised
    /// (`<artifact_root>/<noyau>/<molecule_id>/`). `None` skips both
    /// the mkdir and the `COSMON_ARTIFACT_DIR` env export — used by
    /// the `cs observe` read-path verbs which never need an artifact
    /// dir. Set by [`Self::with_artifact_root`].
    artifact_root: Option<PathBuf>,
    /// Model pin exported as [`env::ANTHROPIC_MODEL`] into every
    /// spawned `cs` subprocess so the worker `claude` it launches runs
    /// the configured model (avatar-surface D1). `None` skips the
    /// export — the worker falls back to whatever the claude CLI
    /// resolves on its own. Set by [`Self::with_claude_model`]; the
    /// value is carried opaquely (no model-id literal lives here).
    claude_model: Option<String>,
}

impl SystemInvoker {
    /// Construct a new invoker bound to the given `cs` binary,
    /// galaxies root, and per-call timeout. No Anthropic key is
    /// injected by default; chain [`Self::with_anthropic_key`] to add
    /// it (the read-path verbs — `observe` — do not need it).
    #[must_use]
    pub fn new(cs_path: PathBuf, galaxies_root: PathBuf, timeout: Duration) -> Self {
        Self {
            cs_path,
            galaxies_root,
            timeout,
            anthropic_api_key: None,
            artifact_root: None,
            claude_model: None,
        }
    }

    /// Inject the boot-resolved Anthropic API key (step 3c) into the
    /// env of every spawned `cs` subprocess, so the worker `claude` it
    /// launches inherits the key. This is the binary equivalent of the
    /// shell script's `export ANTHROPIC_API_KEY` — except the key may
    /// come from a *file* (docker-secret / operator-file), in which
    /// case the adapter's own env never carried it and the child would
    /// otherwise see nothing. `None` is a no-op (the child inherits
    /// whatever the adapter env already holds).
    #[must_use]
    pub fn with_anthropic_key(mut self, key: Option<String>) -> Self {
        self.anthropic_api_key = key;
        self
    }

    /// Configure the artifact root so that every `cs tackle` spawn
    /// (a) `mkdir -p <artifact_root>/<noyau>/<molecule_id>/` before
    /// invoking the binary, and (b) exports `COSMON_ARTIFACT_DIR` to
    /// that path in the child env. The convention is the pact between
    /// the adapter (which knows the noyau + `molecule_id` at admission
    /// time) and the worker (which writes outputs there for the GET
    /// `/artifacts` route to serve later).
    #[must_use]
    pub fn with_artifact_root(mut self, root: Option<PathBuf>) -> Self {
        self.artifact_root = root;
        self
    }

    /// Configure the model pin exported as [`env::ANTHROPIC_MODEL`]
    /// into every spawned `cs` subprocess (avatar-surface D1). The
    /// caller passes the *resolved* value from
    /// [`crate::config::RppConfig::resolved_claude_model`] — config
    /// default, operator override, or `None` for the explicit opt-out.
    /// This builder is the single read-point of the pin on the spawn
    /// path; the value crosses the adapter → `cs tackle` → tmux →
    /// `claude` chain without ever being re-derived.
    #[must_use]
    pub fn with_claude_model(mut self, model: Option<String>) -> Self {
        self.claude_model = model;
        self
    }

    /// Resolve the per-tenant `cwd` for the subprocess. ADR-080 §3.5:
    /// `~/galaxies/<noyau>/`.
    #[must_use]
    pub fn cwd_for_spark(&self, spark: &Spark) -> PathBuf {
        self.galaxies_root.join(spark.noyau.as_str())
    }

    /// Execute one `cs` subprocess with the full envelope.
    ///
    /// # Errors
    ///
    /// Returns one of:
    /// - [`RppRejectReason::SubprocessSpawnFailed`] if `cs` cannot be
    ///   started;
    /// - [`RppRejectReason::SubprocessTimeout`] if the call exceeds
    ///   the configured deadline;
    /// - [`RppRejectReason::SubprocessExitNonZero`] if `cs` returns
    ///   non-zero.
    pub async fn invoke(
        &self,
        spark: &Spark,
        args: &[&str],
    ) -> Result<InvocationResult, RppRejectReason> {
        self.invoke_owned(
            spark,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .await
    }

    /// Owned-string variant of [`Self::invoke`] used by routes that
    /// build a dynamic argument vector from a JSON body (e.g. the
    /// POST `/v1/molecules` handler). Sharing the spawn pipeline is
    /// what keeps the §3.5 envelope honest: only one place sets
    /// `COSMON_API_REQUEST` and the per-tenant `cwd`.
    pub async fn invoke_owned(
        &self,
        spark: &Spark,
        args: &[String],
    ) -> Result<InvocationResult, RppRejectReason> {
        let mut cmd = self.build_command(spark, args);
        let fut = cmd.output();
        let output = match timeout(self.timeout, fut).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Err(RppRejectReason::SubprocessSpawnFailed(e.to_string()));
            }
            Err(_) => return Err(RppRejectReason::SubprocessTimeout(self.timeout)),
        };
        let code = output.status.code().unwrap_or(-1);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Trim the excerpt so absurdly large stderr does not bloat logs.
            let excerpt: String = stderr.chars().take(512).collect();
            return Err(RppRejectReason::SubprocessExitNonZero {
                code,
                stderr_excerpt: excerpt,
            });
        }
        Ok(InvocationResult {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: code,
        })
    }

    /// Assemble the fully-enveloped `Command` for one `cs` subprocess
    /// — set half, strip half, key/model/artifact exports, per-tenant
    /// `cwd` — without spawning it. Factored out of
    /// [`Self::invoke_owned`] so the env/args assembly is unit-testable
    /// with zero I/O (the avatar-surface D1 gate: prove the spawn
    /// *reads the config key*, by inspecting the built env rather than
    /// by running a process).
    fn build_command(&self, spark: &Spark, args: &[String]) -> Command {
        let mut cmd = Command::new(&self.cs_path);
        cmd.args(args.iter().map(String::as_str))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Allow-list half of the §3.5 envelope — see
        // [`PASSTHROUGH_VARS`]. Clear the inherited environment
        // wholesale, then re-admit only the named process-hygiene
        // vars. This comes FIRST: everything the envelope needs
        // (`COSMON_API_*`, `COSMON_STATE_DIR`, `COSMON_ARTIFACT_DIR`,
        // the model pin, the API key) is *set* below from adapter
        // config, and the inverse order would erase the envelope.
        cmd.env_clear();
        for (key, value) in std::env::vars_os() {
            if key.to_str().is_some_and(is_passthrough) {
                cmd.env(key, value);
            }
        }
        // Set half of the §3.5 envelope — the three correlation vars.
        // Deliberately after the clear, and deliberately not readable
        // from the adapter env: a request's identity is minted here,
        // never inherited.
        cmd.env(env::COSMON_API_REQUEST, "1")
            .env(env::COSMON_API_REQUEST_ID, &spark.request_id)
            .env(env::COSMON_API_NUCLEON, &spark.nucleon_id);
        // Step 3c — inject the boot-resolved Anthropic key so the
        // spawned worker `claude` inherits it. Set after the clear,
        // so a config-resolved key (which may come from a file the
        // adapter env never carried) wins over any inherited one.
        if let Some(ref key) = self.anthropic_api_key {
            cmd.env("ANTHROPIC_API_KEY", key);
        }
        // Model pin (avatar-surface D1) — exported alongside the key.
        // The value was resolved from the instance config at boot;
        // `None` (explicit opt-out) skips the export entirely so the
        // claude CLI applies its own default. When no pin is
        // configured the allow-list still lets an inherited
        // `ANTHROPIC_MODEL` through.
        if let Some(ref model) = self.claude_model {
            cmd.env(env::ANTHROPIC_MODEL, model);
        }
        // Artifact dir convention (e653 spec, `task-20260522-ef4f`).
        // If an artifact root is configured AND we have a molecule id
        // on the spark, materialise the per-molecule dir and export
        // its path so the worker writes outputs there. Best-effort:
        // a failed mkdir does not abort the spawn — `cs tackle` will
        // fail later if the dir truly cannot exist, with a clearer
        // error than a missing env var. Comes *after* the clear so the
        // just-set value is not immediately erased — and the variable
        // is absent from the allow-list, so a tenant's dir can never
        // be inherited from another tenant's spawn.
        if let (Some(root), Some(mol_id)) = (&self.artifact_root, spark.molecule_id.as_deref()) {
            let dir = root.join(spark.noyau.as_str()).join(mol_id);
            // `create_dir_all` is idempotent; pre-existing dir is fine.
            let _ = std::fs::create_dir_all(&dir);
            cmd.env("COSMON_ARTIFACT_DIR", &dir);
        }
        // State-dir re-pose (B1 moussage resident, task-20260610-e5f6;
        // fix of the known `tackle 503 — COSMON_STATE_DIR strip
        // divergence`). The clear above drops the inherited variable so
        // a mis-set adapter env can never redirect the child — but the
        // library-direct routes resolve the tenant store as
        // `<galaxies_root>/<noyau>/.cosmon/state` while the subprocess
        // was left to walk-up discovery from `cwd`. Those two paths
        // agree only when nothing else interferes (env override > cwd
        // in `cosmon-filestore`): a resident `cs run` loop, or any `cs`
        // invoked in the container with an inherited COSMON_STATE_DIR,
        // tackles into the wrong store ("no molecule"). Re-posing the
        // variable to the *same* tenant path the library-direct routes
        // use makes the subprocess store deterministic — the exact
        // pattern of the `COSMON_ARTIFACT_DIR` re-set above.
        let tenant_state_dir = self
            .cwd_for_spark(spark)
            .join(cosmon_filestore::resolve::COSMON_DIR_NAME)
            .join("state");
        cmd.env("COSMON_STATE_DIR", &tenant_state_dir);
        let cwd = self.cwd_for_spark(spark);
        // Best-effort cwd: only set if it exists — if the operator hasn't
        // provisioned the tenant galaxy yet the spawn fails with a clear
        // error rather than a confusing "no such file".
        if cwd.exists() {
            cmd.current_dir(&cwd);
        }
        cmd
    }
}

/// Decode a `cs --json` stdout payload. Two acceptable shapes:
///
/// 1. A single JSON value spanning the whole stdout — what the real
///    `cs --json observe :id` emits today (pretty-printed multi-line
///    object). Tried first because it is the canonical shape.
/// 2. NDJSON — one JSON value per line, last non-empty line wins.
///    The cosmon-api convention for streaming variants; the test
///    `fake-cs` binary also emits compact single-line JSON which
///    happens to be a degenerate NDJSON.
///
/// The two-shape acceptance keeps the live deploy honest: the test
/// fixture (`fake-cs` — compact, single line) and the real `cs`
/// (pretty, multi-line) both round-trip without divergence between
/// the integration suite and the docker-compose smoke.
///
/// # Errors
///
/// Returns [`RppRejectReason::SubprocessExitNonZero`] (with code 0
/// to mark this as "process succeeded but output unparseable") if
/// the bytes are not valid UTF-8 or do not parse as JSON in either
/// shape.
pub fn parse_cs_json(stdout: &[u8]) -> Result<Value, RppRejectReason> {
    let s = std::str::from_utf8(stdout).map_err(|e| RppRejectReason::SubprocessExitNonZero {
        code: 0,
        stderr_excerpt: format!("stdout not utf-8: {e}"),
    })?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    // Shape 1 — whole-stdout JSON value (real `cs --json`).
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    // Shape 2 — NDJSON, last non-empty line wins (fake-cs / streaming).
    let line = s
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("{}");
    serde_json::from_str(line).map_err(|e| RppRejectReason::SubprocessExitNonZero {
        code: 0,
        stderr_excerpt: format!("parse cs --json: {e}"),
    })
}

/// Build the argument vector for `cs observe :id --json`. Exposed
/// for unit tests of route-level argument construction.
#[must_use]
pub fn observe_molecule_args(molecule_id: &str) -> Vec<&str> {
    vec!["--json", "observe", molecule_id]
}

/// Build the argument vector for `cs nucleate <formula> ...`.
///
/// Order: `--json nucleate <formula> [--kind <kind>] [--var k=v ...]
/// [--tag t1 ...]`. The `--json` flag is global on the cosmon CLI and
/// MUST precede the subcommand. The variables map is iterated in
/// stable insertion order (each `(key, value)` rendered as
/// `--var key=value`).
///
/// Each entry is owned `String` because the formula and kind/var/tag
/// inputs come from a JSON body that does not survive the subprocess
/// invocation. Tests that exercise the shape independently can
/// construct the vector directly.
#[must_use]
pub fn nucleate_molecule_args(
    formula: &str,
    kind: Option<&str>,
    variables: &[(String, String)],
    tags: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = vec!["--json".into(), "nucleate".into(), formula.to_owned()];
    if let Some(k) = kind {
        args.push("--kind".into());
        args.push(k.to_owned());
    }
    for (k, v) in variables {
        args.push("--var".into());
        args.push(format!("{k}={v}"));
    }
    for t in tags {
        args.push("--tag".into());
        args.push(t.clone());
    }
    args
}

/// Build the argument vector for the resident drain —
/// `cs run <root>` with the binding-derived bounds (design (a),
/// B1 moussage resident).
///
/// THE composition point where the bound-strength rule lands in code: the bounds are
/// read from the tenant's [`crate::nucleon_map::DrainBounds`]
/// (operator-written, BLAKE3-sealed) and turned into `cs run` flags —
/// the request body contributes only the root molecule id, never a
/// bound. `--timeout` makes the loop's deadline a NAMED exit (I4);
/// the drain runs INSIDE the tenant container, co-located with the
/// `StateStore` and `trunk.lock` (a flock only binds holders on the
/// same filesystem — the validity condition of I1).
#[must_use]
pub fn run_molecule_args(
    root_id: &str,
    bounds: &crate::nucleon_map::DrainBounds,
    timeout_secs: u64,
) -> Vec<String> {
    vec![
        "--json".into(),
        "run".into(),
        root_id.to_owned(),
        "--max-actions".into(),
        bounds.budget.to_string(),
        "--max-depth".into(),
        bounds.max_depth.to_string(),
        "--max-molecules".into(),
        bounds.max_molecules.to_string(),
        "--timeout".into(),
        timeout_secs.to_string(),
    ]
}

/// Build the argument vector for `cs tackle <id> --json`.
///
/// T9 remote-tackle V2 — the only §8p verb that re-uses the §3.5 subprocess
/// envelope (the other verbs went library-direct after T-RPP-LIB-DIRECT).
/// `cs tackle` is fundamentally out-of-process: it shells `tmux new`
/// with `claude` inside the per-tenant container. The `--no-attach`
/// flag is added so the subprocess returns promptly with the worker
/// session metadata rather than blocking on the attach.
///
/// `--json` is global on the cosmon CLI and MUST precede the
/// subcommand. `--force` is added so a re-tackle on a freshly
/// nucleated molecule does not collide with a stale tmux pane if one
/// happens to linger (which it should not under normal flow, but the
/// re-entrancy discipline matches the rest of the surface).
#[must_use]
pub fn tackle_molecule_args(molecule_id: &str) -> Vec<String> {
    vec![
        "--json".into(),
        "tackle".into(),
        molecule_id.to_owned(),
        "--force".into(),
    ]
}

/// Helper for the absolute path of the `cs` binary used to set up
/// the [`SystemInvoker`]. Resolves in this order:
///
/// 1. `$COSMON_RPP_CS` env var (escape hatch);
/// 2. the path supplied by the operator config (`rpp.toml`);
/// 3. `cs` on `$PATH`.
#[must_use]
pub fn resolve_cs_path(operator_supplied: Option<&Path>) -> PathBuf {
    if let Some(p) = std::env::var_os("COSMON_RPP_CS") {
        return PathBuf::from(p);
    }
    if let Some(p) = operator_supplied {
        return p.to_path_buf();
    }
    PathBuf::from("cs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nucleon_map::Noyau;
    use std::ffi::OsStr;

    /// Minimal spark for zero-I/O command-assembly tests. No tenant
    /// dir is materialised — `build_command`'s best-effort `cwd` check
    /// simply skips a non-existent path, and nothing is spawned.
    fn spark() -> Spark {
        Spark {
            request_id: "req-d1-model-pin".to_owned(),
            nucleon_id: "nuc-test".to_owned(),
            noyau: Noyau::new("tenant-test"),
            verb: "tackle".to_owned(),
            molecule_id: Some("task-20260610-3791".to_owned()),
            inbox_path: PathBuf::from("/tmp/unused-inbox-path"),
        }
    }

    /// Extract the value set for `key` on the built (un-spawned)
    /// command, if any. `None` covers both "never set" and
    /// "`env_remove`'d".
    fn built_env(cmd: &Command, key: &str) -> Option<String> {
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    // ── avatar-surface D1 — the spawn reads the config key ──────────────
    //
    // Zero-I/O gate: the model pin must flow config → invoker → child
    // env. The tests inspect the assembled `Command` without spawning.

    #[test]
    fn build_command_exports_configured_claude_model() {
        let model = crate::config::RppConfig::default()
            .resolved_claude_model()
            .expect("default config carries the pin");
        let invoker = SystemInvoker::new(
            PathBuf::from("/nonexistent/cs"),
            PathBuf::from("/nonexistent/galaxies"),
            Duration::from_secs(1),
        )
        .with_claude_model(Some(model.clone()));
        let args = tackle_molecule_args("task-20260610-3791");
        let cmd = invoker.build_command(&spark(), &args);
        assert_eq!(
            built_env(&cmd, env::ANTHROPIC_MODEL).as_deref(),
            Some(model.as_str()),
            "the spawn env must carry the config-resolved model pin"
        );
    }

    #[test]
    fn build_command_without_pin_exports_no_model() {
        // Explicit opt-out (`claude_model = ""` → resolved `None`):
        // no env export, the claude CLI applies its own default.
        let invoker = SystemInvoker::new(
            PathBuf::from("/nonexistent/cs"),
            PathBuf::from("/nonexistent/galaxies"),
            Duration::from_secs(1),
        );
        let args = tackle_molecule_args("task-20260610-3791");
        let cmd = invoker.build_command(&spark(), &args);
        assert_eq!(
            built_env(&cmd, env::ANTHROPIC_MODEL),
            None,
            "no pin configured → the variable must not be exported"
        );
    }

    #[test]
    fn build_command_model_pin_survives_strip_loop() {
        // The pin is exported AFTER `env_clear`; this guards the
        // ordering (a re-shuffle that cleared after setting would
        // silently undo the pin, exactly like the COSMON_ARTIFACT_DIR
        // precedent the comment warns about).
        assert!(
            is_passthrough(env::ANTHROPIC_MODEL),
            "ANTHROPIC_MODEL is a set-half var and an inheritable one: \
             an unpinned invoker must still let the adapter's value through"
        );
        let invoker = SystemInvoker::new(
            PathBuf::from("/nonexistent/cs"),
            PathBuf::from("/nonexistent/galaxies"),
            Duration::from_secs(1),
        )
        .with_claude_model(Some("operator-override-model".to_owned()));
        let cmd = invoker.build_command(&spark(), &tackle_molecule_args("m-1"));
        assert_eq!(
            built_env(&cmd, env::ANTHROPIC_MODEL).as_deref(),
            Some("operator-override-model")
        );
    }

    // ── §3.5 allow-list envelope (delib-20260819-cda2, C2) ──────────────
    //
    // The structural invariants of the allow-list, as pure predicates.
    // They hold without spawning anything; the spawn-level proof lives
    // in `tests/subprocess_env_hygiene.rs`.

    #[test]
    fn operator_kill_switch_never_crosses_the_perimeter() {
        // The defect that motivated the deny-list → allow-list change.
        // `COSMON_SKIP_PRE_DONE_HOOK` disarms the blocking `pre_done`
        // gate for every `cs done` that inherits it; it is the human
        // operator's per-invocation gesture, and there is no operator
        // inside the adapter's container.
        assert!(
            !is_passthrough("COSMON_SKIP_PRE_DONE_HOOK"),
            "the pre_done kill-switch must never be inheritable across \
             the adapter perimeter"
        );
    }

    #[test]
    fn no_cosmon_variable_is_inheritable() {
        // The structural property, not a roster check: under an
        // allow-list, a `COSMON_*` reader added anywhere in the
        // workspace is closed on arrival. Every `COSMON_*` the child
        // legitimately needs is SET by `build_command` from adapter
        // config, never inherited.
        for name in PASSTHROUGH_VARS {
            assert!(
                !name.starts_with("COSMON_"),
                "no COSMON_* var may be inheritable; found {name}"
            );
        }
    }

    #[test]
    fn filestore_resolution_vars_are_not_inheritable() {
        // T25 Gap 2 regression guard, restated for the allow-list: an
        // adapter booted with `COSMON_STATE_DIR=/wrong` must not
        // redirect the child's filestore lookups.
        for name in cosmon_filestore::resolve::RESOLUTION_VARS {
            assert!(
                !is_passthrough(name),
                "filestore resolution var {name} must not be inheritable"
            );
        }
    }

    #[test]
    fn pilot_vars_are_not_inheritable() {
        // The worker *pilotage* vars (`CB_DEPTH`, `COSMON_EGRESS_POLICY`,
        // `ANTHROPIC_MODEL`, …) are emitted by `cs tackle` for its own
        // children. An adapter that inherited them would hand one
        // tenant's steering to another's worker. `ANTHROPIC_MODEL` is
        // the documented exception: it is the avatar-surface model pin,
        // re-set from config when one is configured.
        for name in cosmon_core::pilot_env::names() {
            if name == env::ANTHROPIC_MODEL {
                continue;
            }
            assert!(
                !is_passthrough(name),
                "pilot var {name} must not cross the adapter perimeter"
            );
        }
    }

    #[test]
    fn allow_list_has_no_duplicate_names() {
        // A duplicate is the shape a hand-maintained roster rots into:
        // two entries, one reviewed, one not.
        let mut seen = PASSTHROUGH_VARS.to_vec();
        seen.sort_unstable();
        let len_before = seen.len();
        seen.dedup();
        assert_eq!(
            len_before,
            seen.len(),
            "duplicate entry in PASSTHROUGH_VARS"
        );
    }

    #[test]
    fn process_basics_stay_inheritable() {
        // The other failure mode of an allow-list: too narrow, and the
        // child cannot find `git` or read any config at all. These
        // three are load-bearing for every `cs` verb that shells out.
        for name in ["PATH", "HOME", "TMPDIR"] {
            assert!(is_passthrough(name), "{name} must stay inheritable");
        }
    }

    // ── B1 moussage resident (task-20260610-e5f6) — state-dir re-pose ────
    //
    // Fix of the known `tackle 503 — COSMON_STATE_DIR strip divergence`:
    // the subprocess must target the SAME tenant store the
    // library-direct routes resolve (`<galaxies_root>/<noyau>/.cosmon/
    // state`), explicitly, not by walk-up luck.

    #[test]
    fn build_command_reposes_state_dir_to_tenant_store() {
        let invoker = SystemInvoker::new(
            PathBuf::from("/nonexistent/cs"),
            PathBuf::from("/nonexistent/galaxies"),
            Duration::from_secs(1),
        );
        let cmd = invoker.build_command(&spark(), &tackle_molecule_args("m-1"));
        assert_eq!(
            built_env(&cmd, "COSMON_STATE_DIR").as_deref(),
            Some("/nonexistent/galaxies/tenant-test/.cosmon/state"),
            "subprocess must be pinned to the tenant store the \
             library-direct routes use"
        );
    }

    #[test]
    fn state_dir_repose_survives_strip_loop() {
        // COSMON_STATE_DIR is NOT on the allow-list (that is the
        // point: a mis-set adapter env never leaks). The re-pose must
        // therefore come AFTER `env_clear` — this test fails if a
        // future re-shuffle clears after re-posing.
        assert!(
            !is_passthrough("COSMON_STATE_DIR"),
            "COSMON_STATE_DIR must stay off the allow-list — inherited \
             values are never trusted; the re-pose sets the canonical one"
        );
        let invoker = SystemInvoker::new(
            PathBuf::from("/nonexistent/cs"),
            PathBuf::from("/nonexistent/galaxies"),
            Duration::from_secs(1),
        );
        let cmd = invoker.build_command(&spark(), &tackle_molecule_args("m-1"));
        assert!(
            built_env(&cmd, "COSMON_STATE_DIR").is_some(),
            "re-pose must win over the strip loop"
        );
    }

    #[test]
    fn run_args_carry_binding_bounds_server_side() {
        // Design (a) composition point (task-20260610-e5f6): every
        // bound in the spawned `cs run` comes from the binding, the
        // body contributes only the root id. The flags mirror the
        // cs run surface: --max-actions (B3) / --max-depth (B1) /
        // --max-molecules (B2) / --timeout (named deadline, I4).
        let bounds = crate::nucleon_map::DrainBounds {
            budget: 16,
            max_depth: 4,
            max_molecules: 32,
        };
        let args = run_molecule_args("task-20260610-root", &bounds, 600);
        assert_eq!(
            args,
            vec![
                "--json",
                "run",
                "task-20260610-root",
                "--max-actions",
                "16",
                "--max-depth",
                "4",
                "--max-molecules",
                "32",
                "--timeout",
                "600",
            ]
        );
    }

    #[test]
    fn observe_args_emit_json_flag() {
        assert_eq!(
            observe_molecule_args("mol-1"),
            vec!["--json", "observe", "mol-1"]
        );
    }

    #[test]
    fn tackle_args_emit_json_flag_and_force() {
        // T9 remote-tackle V2 (`task-20260512-c6de`). `--force` is part of
        // the canonical shape so a fresh tackle on a freshly nucleated
        // molecule is not blocked by a stale tmux pane.
        let args = tackle_molecule_args("task-20260512-c6de");
        assert_eq!(args[0], "--json");
        assert_eq!(args[1], "tackle");
        assert_eq!(args[2], "task-20260512-c6de");
        assert!(args.contains(&"--force".to_string()));
    }

    #[test]
    fn parse_cs_json_handles_single_object() {
        let v = parse_cs_json(b"{\"a\":1}\n").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_cs_json_handles_ndjson_last_wins() {
        let v = parse_cs_json(b"{\"a\":1}\n{\"a\":2}\n").unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn parse_cs_json_handles_pretty_multiline() {
        // Reproduces the on-disk shape `cs --json observe :id` emits
        // (caught by the V0 docker-compose smoke after the fake-cs
        // fixture happened to mask the divergence).
        let pretty = b"{\n  \"id\": \"task-1\",\n  \"status\": \"completed\"\n}\n";
        let v = parse_cs_json(pretty).unwrap();
        assert_eq!(v["id"], "task-1");
        assert_eq!(v["status"], "completed");
    }

    #[test]
    fn parse_cs_json_treats_blank_stdout_as_empty_object() {
        let v = parse_cs_json(b"   \n").unwrap();
        assert_eq!(v, Value::Object(serde_json::Map::new()));
    }

    #[test]
    fn resolve_cs_path_falls_back_to_supplied_path() {
        // Don't touch env in tests (2024-edition `set_var` is unsafe);
        // this exercises the fallback when no env override is in play.
        let p = std::path::Path::new("/usr/local/bin/cs");
        // If COSMON_RPP_CS happens to be set in the environment we
        // skip — keeps the test deterministic without env mutation.
        if std::env::var_os("COSMON_RPP_CS").is_none() {
            assert_eq!(resolve_cs_path(Some(p)), p.to_path_buf());
        }
    }

    #[test]
    fn resolve_cs_path_default_is_cs_on_path() {
        if std::env::var_os("COSMON_RPP_CS").is_none() {
            assert_eq!(resolve_cs_path(None), PathBuf::from("cs"));
        }
    }
}
