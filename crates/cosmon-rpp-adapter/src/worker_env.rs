// SPDX-License-Identifier: AGPL-3.0-only

//! The worker envelope — the ADR-080 §3.5 environment discipline,
//! re-homed at the transport seam (issue #54 / U6).
//!
//! # What replaced clause (e)
//!
//! Until U6 the adapter shelled the `cs` binary for `tackle`, `run` and
//! `land`, and the §3.5 clause (e) *subprocess envelope*
//! (`SystemInvoker`) governed the env of that child. The subprocess is
//! gone: dispatch now runs in-process through
//! [`cosmon_runtime::tackle_exec::LibraryExecutor`] over the
//! [`cosmon_core::transport::TransportBackend`] port. The one place a
//! child process still crosses the adapter perimeter is the **worker
//! spawn itself** — the `claude`/`codex` session the transport backend
//! opens in tmux — and that is where the envelope's hygiene half must
//! now hold.
//!
//! The property protected here is unchanged from
//! delib-20260819-cda2 C2: *nothing of the adapter's own environment
//! reaches a worker unless its name is written on an explicit
//! allow-list, with a reason.* The mechanism moved: instead of
//! `Command::env_clear` on a `cs` child, [`EnvelopedBackend`] rewrites
//! every spawned agent command into `/usr/bin/env -i K=V… <command>`,
//! so the worker's environment is exactly the enveloped set regardless
//! of what the tmux *server* (started by the adapter, so carrying the
//! adapter's env) would otherwise hand it.
//!
//! # The set half
//!
//! What the envelope sets, and why (see [`WorkerEnvelope::build_env`]):
//!
//! - `COSMON_EGRESS_EXPOSED=1` — the ADR-155 re-homing. The retired
//!   subprocess envelope carried `COSMON_API_REQUEST=1`, and the macOS
//!   seatbelt / egress fail-closed posture keyed on that marker being
//!   present. The marker itself CANNOT ride on the worker env: `cs`
//!   refuses the operator-only lifecycle verbs (`evolve`, `complete`,
//!   `done`) at parse time whenever `COSMON_API_REQUEST=1`
//!   ([`cosmon_core::api_envelope`] lock 2), and those verbs are the
//!   worker's whole job — which is exactly why `cs tackle`'s own
//!   hand-off consumed the marker before spawning a worker. The
//!   dedicated operator knob
//!   [`cosmon_core::egress::EXPOSED_MULTITENANT_ENV`] exists for this
//!   signal (task-20260713-8acc) and carries it without the verb-lock
//!   side effect, so every worker the adapter dispatches evaluates its
//!   egress posture as *exposed multi-tenant*, fail-closed.
//! - `COSMON_STATE_DIR` — pinned to the tenant store
//!   (`<tenant_root>/.cosmon/state`), the same deterministic re-pose
//!   the subprocess envelope performed (B1 moussage resident,
//!   task-20260610-e5f6): walk-up discovery from the worktree agrees
//!   with this value only when nothing else interferes, and an
//!   inherited adapter value must never win.
//! - `COSMON_ARTIFACT_DIR` — the per-molecule delivery window (e653
//!   spec): the worker writes its canonical output there and the
//!   `GET /artifacts` routes serve from it.
//! - `ANTHROPIC_API_KEY` — the boot-resolved key (docker-secret /
//!   operator-file ladder), which the adapter env may never have
//!   carried; set explicitly so the worker `claude` inherits it.
//! - `ANTHROPIC_MODEL` — the avatar-surface D1 model pin, when the
//!   instance config carries one.
//!
//! `COSMON_API_REQUEST` / `COSMON_API_REQUEST_ID` are deliberately
//! **absent**: the worker is not the network request (ADR-080 §3.5's
//! hand-off semantics, [`cosmon_core::api_envelope`]), and correlation
//! of the dispatch to the HTTP request lives in the adapter's own
//! audit inbox and the `WorkerSpawned` event, not in the worker's env.

use std::path::PathBuf;

use cosmon_core::id::WorkerId;
use cosmon_core::injection::InjectionProvenance;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};

/// Env var names the envelope owns. The correlation pair of the retired
/// subprocess envelope (`COSMON_API_REQUEST*`) is intentionally not
/// re-declared here — see the module docs for where each duty went.
pub mod env {
    /// Model pin for the worker `claude` session (avatar-surface D1).
    /// The value comes from the instance config
    /// ([`crate::config::RppConfig::resolved_claude_model`]) — this
    /// crate never holds a model-id literal outside that config module.
    pub const ANTHROPIC_MODEL: &str = "ANTHROPIC_MODEL";
}

/// Allow-list half of the worker envelope — the **only** variables of
/// the adapter's own environment that may cross the adapter→worker
/// boundary.
///
/// **Why an allow-list.** This set used to be a deny-list
/// (`STRIP_VARS`): everything was inherited except a hand-maintained
/// roster of `COSMON_*` resolution vars. A security envelope built that
/// way is wrong by default for every variable added anywhere else in
/// the code — the omission is not an oversight, it is the normal
/// behaviour of the structure. The concrete falsification
/// (delib-20260819-cda2, C2) was `COSMON_SKIP_PRE_DONE_HOOK`: the
/// human operator's kill-switch for the blocking `pre_done` gate,
/// absent from the deny-list, and therefore inherited by every worker
/// and every `cs done` those workers ran. One variable set once — an
/// image entrypoint, a service wrapper, an operations shell — disarmed
/// the Definition-of-Done of every subsequent harvest, inside a
/// container where no operator exists to make the gesture.
///
/// Under an allow-list the default is reversed: a variable reaches the
/// worker only because its name is written here, with a reason. A new
/// `COSMON_*` reader elsewhere in the workspace is closed on arrival,
/// not on the next audit.
///
/// **What is not here.** No `COSMON_*` name at all. Everything the
/// worker legitimately needs (`COSMON_STATE_DIR`,
/// `COSMON_ARTIFACT_DIR`, the exposed-posture knob, the model pin, the
/// API key) is *set* explicitly by [`WorkerEnvelope::build_env`] after
/// the clear, from adapter config — never inherited. Everything else
/// (`COSMON_SKIP_PRE_DONE_HOOK`, `COSMON_OPERATOR_GESTURE`,
/// `COSMON_GALAXY`, `CB_DEPTH`, the pilot vars, …) is simply gone.
///
/// **What is here, and why.** Only process-hygiene variables a POSIX
/// child and the tools a worker shells (git, tmux, sh, claude) cannot
/// work without. Each line is a deliberate hole in the envelope;
/// adding one is a security decision, which is exactly the property
/// the deny-list did not have.
pub const PASSTHROUGH_VARS: &[&str] = &[
    // Process basics. Without `PATH` the worker cannot find `git`,
    // `sh` or `claude`; without `HOME` git and the claude CLI read no
    // configuration at all.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "PWD",
    // Terminal / locale — workers live in tmux panes, and UTF-8 paths
    // round-trip only with a sane locale.
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    // XDG bases: where the worker's own tooling keeps per-user state.
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    // Git transport over ssh (agent socket) — a worker that cannot
    // authenticate cannot push a branch.
    "SSH_AUTH_SOCK",
    // Egress proxy configuration, when the deployment has one. Absent
    // from the worker, every network call inside the container would
    // bypass the proxy and fail closed at the jail.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // Anthropic credentials / endpoint for the worker `claude`.
    // `ANTHROPIC_API_KEY` is additionally *set* from config when the
    // boot ladder resolved one (the key may live in a file the adapter
    // env never carried); this entry covers the plain-inheritance
    // deployment. `ANTHROPIC_MODEL` is likewise re-set by the pin when
    // one is configured.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    env::ANTHROPIC_MODEL,
    // Diagnostics for the worker itself. Read-only knobs; they steer no
    // resolution and grant no capability.
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// Whether `key` is allowed to be inherited by a spawned worker.
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

/// Per-dispatch inputs of the set half of the envelope.
///
/// Built by the tackle / run routes from the admitted request and the
/// adapter's boot-resolved config, then compiled into the concrete env
/// list by [`Self::build_env`] and clamped onto every spawn by
/// [`EnvelopedBackend`].
#[derive(Debug, Clone)]
pub struct WorkerEnvelope {
    /// Tenant galaxy root (`<galaxies_root>/<noyau>`), from which the
    /// deterministic `COSMON_STATE_DIR` re-pose is derived.
    pub tenant_root: PathBuf,
    /// Per-molecule artifact directory exported as
    /// `COSMON_ARTIFACT_DIR`; `None` skips the export.
    pub artifact_dir: Option<PathBuf>,
    /// Boot-resolved Anthropic key (docker-secret → operator-file →
    /// env ladder); `None` falls back to plain allow-list inheritance.
    pub anthropic_api_key: Option<String>,
    /// Avatar-surface D1 model pin; `None` is the explicit opt-out.
    pub claude_model: Option<String>,
}

impl WorkerEnvelope {
    /// Compile the envelope over a snapshot of the parent environment.
    ///
    /// Pure over the injected `parent` iterator so the whole hygiene
    /// invariant is testable without mutating the process env or
    /// spawning anything: allow-listed names pass through, everything
    /// else is dropped, then the set half lands **after** the clear so
    /// a leaked adapter value can never win over the canonical one.
    #[must_use]
    pub fn build_env<I>(&self, parent: I) -> Vec<(String, String)>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut out: Vec<(String, String)> = parent
            .into_iter()
            .filter(|(k, _)| is_passthrough(k))
            .collect();
        // Set half — every entry overrides an inherited value by
        // construction: `set` removes any allow-listed duplicate first.
        let mut set = |key: &str, value: String| {
            out.retain(|(k, _)| k != key);
            out.push((key.to_owned(), value));
        };
        // ADR-155 re-homing: the worker evaluates its egress posture as
        // exposed multi-tenant, fail-closed. See the module docs for
        // why this is the dedicated knob and not `COSMON_API_REQUEST`.
        set(cosmon_core::egress::EXPOSED_MULTITENANT_ENV, "1".to_owned());
        // Deterministic tenant-store pin (B1 moussage resident).
        let state_dir = self
            .tenant_root
            .join(cosmon_filestore::resolve::COSMON_DIR_NAME)
            .join("state");
        set("COSMON_STATE_DIR", state_dir.to_string_lossy().into_owned());
        if let Some(dir) = &self.artifact_dir {
            set("COSMON_ARTIFACT_DIR", dir.to_string_lossy().into_owned());
        }
        if let Some(key) = &self.anthropic_api_key {
            set("ANTHROPIC_API_KEY", key.clone());
        }
        if let Some(model) = &self.claude_model {
            set(env::ANTHROPIC_MODEL, model.clone());
        }
        out
    }
}

/// Transport-port decorator that clamps the worker envelope onto every
/// spawn.
///
/// [`cosmon_runtime::tackle_exec::LibraryExecutor`] spawns through
/// whatever [`TransportBackend`] it is given; this wrapper is the seam
/// where the adapter's §3.5 duty lands. `spawn` rewrites the agent's
/// command into `/usr/bin/env -i K=V… <command> <args…>`, so the
/// worker process receives exactly the enveloped environment — the
/// tmux server's env (which is the adapter's) never reaches it. Every
/// other port method delegates untouched.
///
/// The `env -i` prefix survives the shell quoting the tmux backend
/// applies (each argument is quoted individually) and the pane-command
/// normaliser in `cosmon-transport` already strips env-assignment
/// prefixes when matching panes, so presence probes keep working.
#[derive(Debug, Clone)]
pub struct EnvelopedBackend<B> {
    inner: B,
    env: Vec<(String, String)>,
}

impl<B> EnvelopedBackend<B> {
    /// Wrap `inner`, compiling the envelope over the adapter's current
    /// process environment.
    #[must_use]
    pub fn new(inner: B, envelope: &WorkerEnvelope) -> Self {
        Self {
            inner,
            env: envelope.build_env(std::env::vars()),
        }
    }

    /// The compiled `(key, value)` list this decorator clamps onto
    /// every spawn — exposed for the hygiene falsifiers.
    #[must_use]
    pub fn compiled_env(&self) -> &[(String, String)] {
        &self.env
    }
}

impl<B: TransportBackend> TransportBackend for EnvelopedBackend<B> {
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        let mut args: Vec<String> = Vec::with_capacity(2 + self.env.len() + agent.args.len());
        args.push("-i".to_owned());
        for (k, v) in &self.env {
            args.push(format!("{k}={v}"));
        }
        args.push(agent.command.clone());
        args.extend(agent.args.iter().cloned());
        let enveloped = AgentDefinition {
            id: agent.id.clone(),
            role: agent.role,
            command: "/usr/bin/env".to_owned(),
            args,
        };
        self.inner.spawn(&enveloped, config)
    }

    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        self.inner.terminate(id)
    }

    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        self.inner.is_alive(id)
    }

    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        self.inner.send_input(id, input)
    }

    fn send_input_observed(
        &self,
        id: &WorkerId,
        input: &str,
        provenance: &InjectionProvenance,
    ) -> Result<(), TransportError> {
        self.inner.send_input_observed(id, input, provenance)
    }

    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        self.inner.capture_output(id, lines)
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        self.inner.list_sessions()
    }

    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        self.inner.graceful_exit(id, timeout)
    }

    fn session_exists(&self, session_name: &str) -> Result<bool, TransportError> {
        self.inner.session_exists(session_name)
    }

    fn terminate_session(&self, session_name: &str) -> Result<(), TransportError> {
        self.inner.terminate_session(session_name)
    }
}

/// Object-safe handle over the adapter's shared transport backend.
///
/// [`crate::AppState`] holds the backend as
/// `Arc<dyn TransportBackend + Send + Sync>` (production: the tmux
/// backend; tests: the in-memory mock), while
/// [`cosmon_runtime::tackle_exec::LibraryExecutor`] wants a concrete
/// `B: TransportBackend`. This newtype bridges the two by delegating
/// every port method through the `Arc`.
#[derive(Clone)]
pub struct SharedBackend(pub std::sync::Arc<dyn TransportBackend + Send + Sync>);

impl std::fmt::Debug for SharedBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedBackend(..)")
    }
}

impl TransportBackend for SharedBackend {
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        self.0.spawn(agent, config)
    }

    fn terminate(&self, id: &WorkerId) -> Result<(), TransportError> {
        self.0.terminate(id)
    }

    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        self.0.is_alive(id)
    }

    fn send_input(&self, id: &WorkerId, input: &str) -> Result<(), TransportError> {
        self.0.send_input(id, input)
    }

    fn send_input_observed(
        &self,
        id: &WorkerId,
        input: &str,
        provenance: &InjectionProvenance,
    ) -> Result<(), TransportError> {
        self.0.send_input_observed(id, input, provenance)
    }

    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        self.0.capture_output(id, lines)
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        self.0.list_sessions()
    }

    fn graceful_exit(
        &self,
        id: &WorkerId,
        timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        self.0.graceful_exit(id, timeout)
    }

    fn session_exists(&self, session_name: &str) -> Result<bool, TransportError> {
        self.0.session_exists(session_name)
    }

    fn terminate_session(&self, session_name: &str) -> Result<(), TransportError> {
        self.0.terminate_session(session_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(root: &std::path::Path) -> WorkerEnvelope {
        WorkerEnvelope {
            tenant_root: root.join("a"),
            artifact_dir: Some(root.join("artifacts")),
            anthropic_api_key: None,
            claude_model: None,
        }
    }

    fn build(parent: &[(&str, &str)]) -> Vec<(String, String)> {
        let root = std::path::Path::new("/galaxies");
        envelope(root).build_env(
            parent
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        )
    }

    fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    // ── §3.5 allow-list envelope (delib-20260819-cda2, C2) ──────────────

    #[test]
    fn operator_kill_switch_never_crosses_the_perimeter() {
        // The defect that motivated the deny-list → allow-list change.
        assert!(!is_passthrough("COSMON_SKIP_PRE_DONE_HOOK"));
        let env = build(&[("COSMON_SKIP_PRE_DONE_HOOK", "1"), ("PATH", "/bin")]);
        assert!(value(&env, "COSMON_SKIP_PRE_DONE_HOOK").is_none());
    }

    #[test]
    fn no_cosmon_variable_is_inheritable() {
        // The structural property, not a roster check: every `COSMON_*`
        // the worker legitimately needs is SET by `build_env` from
        // adapter config, never inherited.
        for name in PASSTHROUGH_VARS {
            assert!(
                !name.starts_with("COSMON_"),
                "no COSMON_* var may be inheritable; found {name}"
            );
        }
    }

    #[test]
    fn filestore_resolution_vars_are_not_inheritable() {
        for name in cosmon_filestore::resolve::RESOLUTION_VARS {
            assert!(
                !is_passthrough(name),
                "filestore resolution var {name} must not be inheritable"
            );
        }
    }

    #[test]
    fn pilot_vars_are_not_inheritable() {
        // `ANTHROPIC_MODEL` is the documented exception: it is the
        // avatar-surface model pin, re-set from config when configured.
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
        // worker cannot find `git` or read any config at all.
        let env = build(&[("PATH", "/usr/bin:/bin"), ("HOME", "/home/user")]);
        assert_eq!(value(&env, "PATH"), Some("/usr/bin:/bin"));
        assert_eq!(value(&env, "HOME"), Some("/home/user"));
    }

    // ── set half ─────────────────────────────────────────────────────────

    #[test]
    fn state_dir_is_reposed_to_the_tenant_store() {
        // Stronger than absence: a leaked adapter COSMON_STATE_DIR is
        // replaced by the canonical per-tenant path, deterministically.
        let env = build(&[("COSMON_STATE_DIR", "/leaked/by/adapter")]);
        assert_eq!(
            value(&env, "COSMON_STATE_DIR"),
            Some("/galaxies/a/.cosmon/state")
        );
    }

    #[test]
    fn exposed_posture_is_stamped_on_every_worker() {
        // ADR-155 re-homing: the retired subprocess envelope's
        // `COSMON_API_REQUEST` exposed-host duty now rides the
        // dedicated knob — and the marker itself must NOT appear, or
        // the worker's own `cs evolve` / `cs complete` would be refused
        // by the §3.5 second lock.
        let env = build(&[]);
        assert_eq!(
            value(&env, cosmon_core::egress::EXPOSED_MULTITENANT_ENV),
            Some("1")
        );
        assert!(value(&env, cosmon_core::api_envelope::REQUEST_ENV).is_none());
        assert!(value(&env, cosmon_core::api_envelope::REQUEST_ID_ENV).is_none());
    }

    #[test]
    fn config_resolved_key_and_pin_win_over_inherited_values() {
        let root = std::path::Path::new("/galaxies");
        let mut envl = envelope(root);
        envl.anthropic_api_key = Some("file-resolved-key".to_owned());
        envl.claude_model = Some("operator-override-model".to_owned());
        let env = envl.build_env(
            [
                ("ANTHROPIC_API_KEY".to_owned(), "inherited-key".to_owned()),
                (
                    env::ANTHROPIC_MODEL.to_owned(),
                    "inherited-model".to_owned(),
                ),
            ]
            .into_iter(),
        );
        assert_eq!(value(&env, "ANTHROPIC_API_KEY"), Some("file-resolved-key"));
        assert_eq!(
            value(&env, env::ANTHROPIC_MODEL),
            Some("operator-override-model")
        );
        // No duplicates: the set half replaces, never shadows.
        assert_eq!(
            env.iter().filter(|(k, _)| k == "ANTHROPIC_API_KEY").count(),
            1
        );
    }

    #[test]
    fn unpinned_envelope_lets_inherited_model_through() {
        let env = build(&[(env::ANTHROPIC_MODEL, "ambient-model")]);
        assert_eq!(value(&env, env::ANTHROPIC_MODEL), Some("ambient-model"));
    }

    #[test]
    fn artifact_dir_is_exported_when_configured() {
        let env = build(&[]);
        assert_eq!(
            value(&env, "COSMON_ARTIFACT_DIR"),
            Some("/galaxies/artifacts")
        );
    }
}
