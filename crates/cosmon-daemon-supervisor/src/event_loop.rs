// SPDX-License-Identifier: AGPL-3.0-only

//! Composition-root event loop for the supervisor binary.
//!
//! The loop is a flat `tokio::select!` over the five inputs the supervisor
//! cares about:
//!
//! | Input | Why |
//! |-------|-----|
//! | `SIGTERM` on the supervisor | operator / launchd shutdown → quiesce |
//! | `SIGCHLD` delivery | a child died → reap and possibly respawn |
//! | config file changed | hot-reload → recompute diff + apply |
//! | kill-switch touched | global mute → SIGTERM every child |
//! | throttle tick (1s) | exit throttle window → respawn |
//!
//! The event loop delegates **every** decision to pure helpers in
//! [`crate::policy`] and [`crate::reload`]: the moment a real event
//! lands it translates to "call `respawn_decision`", "call `diff`", or
//! "call `kill_switch_decision`", never to ad-hoc logic inside the loop.
//! That discipline is what lets the loop stay under ~200 lines despite
//! the number of concurrent inputs.
//!
//! ## Supervised child table
//!
//! Inside the loop we keep a per-name [`ChildRecord`] — a flat enum over
//! the typestate. The `Child<S>` typestate is great for compile-time
//! transition checks, but a `HashMap<String, Child<S>>` can't be built
//! (heterogeneous `S`). So we store the type-erased enum and materialize
//! the typestate on demand inside the helpers that own the transition.
//!
//! ## R3 mitigation (double-spawn-on-restart)
//!
//! On boot we load `state.json`. For every entry with a recorded pid we
//! probe `kill(pid, 0)`: if the OS reports the pid alive, we treat it as
//! "already-managed" (populate the `Running` record, skip the spawn).
//! If the pid is gone we clear the entry and fall through to normal
//! spawn-on-start. See `tests/double_spawn_on_restart.rs`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time;

use crate::adapters::tokio_process::pid_is_alive;
use crate::adapters::{FileStatePort, NotifyConfigWatchPort, TokioProcessPort};
use crate::config::{Config, DaemonSpec};
use crate::model::ChildStatus;
use crate::policy::{
    crash_loop_alert, kill_switch_decision, prune_crash_times, respawn_decision, throttle_deadline,
    RespawnDecision,
};
use crate::ports::{PersistedChild, ProcessPort, Signal, StatePort, SupervisorState};
use crate::reload::diff;

/// Default grace window between SIGTERM and SIGKILL on shutdown.
///
/// Empirically, well-behaved daemons (emacs-daemon, tg-bot, almanac) reach
/// termination well under one second. 5 s is the same figure launchd's
/// `ExitTimeOut` defaults to, which keeps operator expectations aligned.
pub const DEFAULT_TERM_GRACE: StdDuration = StdDuration::from_secs(5);

/// Composite supervisor-level errors surfaced to the binary.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// Config load/parse/validation failed.
    #[error("config: {0}")]
    Config(#[from] crate::config::ConfigError),

    /// State file I/O failure.
    #[error("state: {0}")]
    State(#[from] crate::ports::StateError),

    /// Process-port failure.
    #[error("process: {0}")]
    Process(#[from] crate::ports::ProcessError),

    /// Config watcher failure.
    #[error("watch: {0}")]
    Watch(#[from] crate::ports::ConfigWatchError),

    /// Miscellaneous I/O on the kill-switch / home path.
    #[error("io: {0}")]
    Io(String),
}

// ---------------------------------------------------------------------------
// Supervised child table
// ---------------------------------------------------------------------------

/// One supervised child, flattened out of the typestate for heterogeneous
/// storage. The moment we need the compile-time guarantees the typestate
/// exposes, we pattern-match and rebuild the typed `Child<S>` — see the
/// bottom of this file for the helpers.
#[derive(Debug, Clone)]
struct ChildRecord {
    name: String,
    spec: DaemonSpec,
    status: ChildStatus,
    pid: Option<u32>,
    last_exit_code: Option<i32>,
    last_spawn_at: Option<DateTime<Utc>>,
    last_exit_at: Option<DateTime<Utc>>,
    /// For `Throttling`: when we may respawn.
    throttle_until: Option<DateTime<Utc>>,
    respawn_count: u32,
    /// **Crash-loop escape valve.** Wall-clock times of
    /// recent unexpected exits, pruned to the rolling
    /// `crash_loop_window`. In-memory only (not persisted): a supervisor
    /// restart is itself a fresh start, and the "dead all night"
    /// scenario is a child crash-looping under a *live* supervisor.
    crash_times: Vec<DateTime<Utc>>,
    /// Latch so the `PropulsionDown` alert fires once per crash-loop
    /// episode, not on every crash past the threshold. Cleared when the
    /// recent-crash count falls back below the threshold (the child
    /// recovered), re-arming the valve for a future loop.
    propulsion_alerted: bool,
}

impl ChildRecord {
    fn fresh(spec: DaemonSpec) -> Self {
        Self {
            name: spec.name.clone(),
            spec,
            status: ChildStatus::Exited,
            pid: None,
            last_exit_code: None,
            last_spawn_at: None,
            last_exit_at: None,
            throttle_until: None,
            respawn_count: 0,
            crash_times: Vec::new(),
            propulsion_alerted: false,
        }
    }

    fn to_persisted(&self) -> PersistedChild {
        PersistedChild {
            name: self.name.clone(),
            status: self.status,
            pid: self.pid,
            last_exit_code: self.last_exit_code,
            last_spawn_at: self.last_spawn_at,
            last_exit_at: self.last_exit_at,
            respawn_count: self.respawn_count,
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor — the orchestrator
// ---------------------------------------------------------------------------

/// The resident supervisor. Owns every adapter and the child table.
///
/// Held by `run()` inside the binary. Exposed as `pub` so integration tests
/// can drive it directly from tokio (`Supervisor::step_once`).
pub struct Supervisor {
    process: TokioProcessPort,
    state: FileStatePort,
    kill_switch_path: PathBuf,
    children: HashMap<String, ChildRecord>,
    /// mpsc receiver fed by the debounced notify watcher (bridged through
    /// `spawn_blocking`).
    config_rx: mpsc::UnboundedReceiver<()>,
    /// Guards keep the watcher alive.
    _watcher_guard: crate::adapters::notify_watcher::WatcherGuard,
    config_path: PathBuf,
    term_grace: StdDuration,
    /// **Crash-loop escape valve.** K consecutive
    /// crash-restarts of a single child inside [`Self::crash_loop_window`]
    /// trip one operator-visible `PropulsionDown` alert. `0` disables.
    crash_loop_threshold: u32,
    /// Rolling window over which [`Self::crash_loop_threshold`] crashes
    /// trip the alert.
    crash_loop_window: chrono::Duration,
    /// Argv the supervisor shells out to surface the `PropulsionDown` alert
    /// (default `["cs", "notify"]`). Empty disables dispatch (still logged).
    notify_command: Vec<String>,
}

impl Supervisor {
    /// Construct the supervisor from the paths the binary resolved.
    ///
    /// Performs first-boot state recovery (R3 mitigation) and opens every
    /// adapter. The real event loop is driven by [`Self::run`].
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] if the config cannot be loaded, the
    /// state file cannot be read, or the notify watcher cannot subscribe
    /// to the config path.
    pub fn new(
        config_path: PathBuf,
        state_path: &Path,
        kill_switch_path: PathBuf,
    ) -> Result<Self, SupervisorError> {
        let cfg = Config::load(&config_path)?;

        let state_port = FileStatePort::new(state_path);
        let persisted = state_port.load()?;

        let process = TokioProcessPort::new();
        let mut children: HashMap<String, ChildRecord> = HashMap::new();

        // Seed child table from config order, overlaying state.
        for spec in &cfg.daemons {
            let mut rec = ChildRecord::fresh(spec.clone());
            if let Some(p) = persisted.children.get(&spec.name) {
                rec.status = p.status;
                rec.last_exit_code = p.last_exit_code;
                rec.last_spawn_at = p.last_spawn_at;
                rec.last_exit_at = p.last_exit_at;
                rec.respawn_count = p.respawn_count;
                // R3 probe: if the persisted pid looks alive, carry it;
                // otherwise drop it so we'll spawn fresh.
                rec.pid = match p.pid {
                    Some(pid) if pid_is_alive(pid) => Some(pid),
                    _ => None,
                };
                if rec.pid.is_some() {
                    rec.status = ChildStatus::Running;
                } else if matches!(rec.status, ChildStatus::Running | ChildStatus::Spawning) {
                    // State said Running but the pid is gone — escalate to
                    // Exited so the normal respawn logic picks up.
                    rec.status = ChildStatus::Exited;
                    rec.last_exit_at = Some(Utc::now());
                }
            }
            children.insert(spec.name.clone(), rec);
        }

        // Set up the notify watcher + bridge thread.
        let watcher = NotifyConfigWatchPort::new(&config_path)?;
        let (std_rx, guard) = watcher.into_parts();

        let (tokio_tx, tokio_rx) = mpsc::unbounded_channel::<()>();
        std::thread::Builder::new()
            .name("daemon-supervisor-config-bridge".into())
            .spawn(move || {
                while std_rx.recv().is_ok() {
                    if tokio_tx.send(()).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| SupervisorError::Io(format!("spawn config bridge: {e}")))?;

        let crash_loop_threshold = cfg.supervisor.crash_loop_threshold;
        let crash_loop_window = chrono::Duration::seconds(
            i64::try_from(cfg.supervisor.crash_loop_window_seconds).unwrap_or(i64::MAX),
        );
        let notify_command = cfg.supervisor.notify_command.clone();

        Ok(Self {
            process,
            state: state_port,
            kill_switch_path,
            children,
            config_rx: tokio_rx,
            _watcher_guard: guard,
            config_path,
            term_grace: DEFAULT_TERM_GRACE,
            crash_loop_threshold,
            crash_loop_window,
            notify_command,
        })
    }

    /// Persist the current child table to the state file atomically.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError::State`] if the filestore adapter fails.
    pub fn persist(&mut self) -> Result<(), SupervisorError> {
        let mut out = SupervisorState {
            version: 1,
            children: BTreeMap::new(),
        };
        for rec in self.children.values() {
            out.children.insert(rec.name.clone(), rec.to_persisted());
        }
        self.state.save(&out)?;
        Ok(())
    }

    /// Evaluate every child once: spawn due respawns, SIGTERM on kill-switch,
    /// reap exited PIDs. Writes state after the pass.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] on adapter failure (spawn, state save,
    /// signal delivery).
    pub fn step_once(&mut self) -> Result<(), SupervisorError> {
        let now = Utc::now();
        let global_mute = self.kill_switch_path.exists();

        // 1. Reap whatever children are alive.
        //
        // Two cases:
        //  (a) we own the `tokio::process::Child` handle — use `reap` to
        //      collect the exit code authoritatively.
        //  (b) we inherited the pid from a prior supervisor incarnation
        //      (R3 mitigation path) — we *don't* own the handle, so
        //      `reap` would return `UnknownPid`. Fall back to
        //      `pid_is_alive` which just asks the OS.
        let pids: Vec<(String, u32)> = self
            .children
            .iter()
            .filter_map(|(name, rec)| rec.pid.map(|pid| (name.clone(), pid)))
            .collect();
        for (name, pid) in pids {
            // Whether this reap observed an *unexpected* exit — i.e. a
            // crash the supervisor will respawn. Fed to the crash-loop
            // escape valve after the borrow on `children` is released.
            let mut crashed = false;
            if self.process.has_child(pid) {
                match self.process.reap(pid)? {
                    crate::ports::ReapOutcome::Alive => {}
                    crate::ports::ReapOutcome::Exited(code) => {
                        if let Some(rec) = self.children.get_mut(&name) {
                            rec.status = ChildStatus::Exited;
                            rec.pid = None;
                            rec.last_exit_code = Some(code);
                            rec.last_exit_at = Some(now);
                            crashed = true;
                        }
                    }
                    crate::ports::ReapOutcome::Signaled => {
                        if let Some(rec) = self.children.get_mut(&name) {
                            rec.status = ChildStatus::Exited;
                            rec.pid = None;
                            rec.last_exit_code = None;
                            rec.last_exit_at = Some(now);
                            crashed = true;
                        }
                    }
                }
            } else if !pid_is_alive(pid) {
                // Inherited pid is gone — flip to Exited so the next
                // respawn branch fires.
                if let Some(rec) = self.children.get_mut(&name) {
                    rec.status = ChildStatus::Exited;
                    rec.pid = None;
                    rec.last_exit_at = Some(now);
                    crashed = true;
                }
            }
            if crashed {
                self.record_crash(&name, now);
            }
            // Inherited and still alive → leave the record as Running;
            // we'll discover its death via a future `pid_is_alive` probe
            // (or, on macOS/Linux, a SIGCHLD if the parent we inherited
            // from was us — but we dropped that Child handle already).
        }

        // 2. For each child, decide what to do next.
        let names: Vec<String> = self.children.keys().cloned().collect();
        for name in names {
            let rec = match self.children.get(&name) {
                Some(r) => r.clone(),
                None => continue,
            };
            let daemon_mute = rec
                .spec
                .kill_switch
                .as_deref()
                .is_some_and(|p| Path::new(p).exists());
            let decision = kill_switch_decision(&rec.spec, global_mute, daemon_mute);

            // If muted and a pid is still alive → SIGTERM it.
            if decision.is_muted() {
                if let Some(pid) = rec.pid {
                    let _ = self.process.signal(pid, Signal::Term);
                    // The next reap pass will observe it exited.
                }
                continue;
            }

            // Not muted — decide based on status.
            match rec.status {
                ChildStatus::Running | ChildStatus::Spawning => {
                    // Already running; nothing to do until SIGCHLD.
                }
                ChildStatus::Exited => {
                    let exited_at = rec.last_exit_at.unwrap_or(now);
                    let action = respawn_decision(&rec.spec, exited_at, now, decision);
                    match action {
                        RespawnDecision::SpawnNow => {
                            self.spawn_child(&name, now);
                        }
                        RespawnDecision::ThrottleUntil(until) => {
                            if let Some(r) = self.children.get_mut(&name) {
                                r.status = ChildStatus::Throttling;
                                r.throttle_until = Some(until);
                            }
                        }
                        RespawnDecision::Quiesce => {}
                    }
                }
                ChildStatus::Throttling => {
                    // `throttle_until` is in-memory only; after a supervisor
                    // restart it comes back as `None`. Re-anchor on the
                    // persisted `last_exit_at` so a resumed throttle honors
                    // the same `spec.throttle_seconds` from the same clock
                    // point. If neither is available (fresh reload with no
                    // history), respawn immediately — the diff pass already
                    // decided the daemon should be present.
                    let until = rec.throttle_until.unwrap_or_else(|| {
                        rec.last_exit_at
                            .map_or(now, |t| throttle_deadline(&rec.spec, t))
                    });
                    if now >= until {
                        self.spawn_child(&name, now);
                    }
                }
                ChildStatus::Respawning => {
                    self.spawn_child(&name, now);
                }
            }
        }

        self.persist()?;
        Ok(())
    }

    /// Attempt to spawn a child. **Per-daemon spawn failures are non-fatal.**
    ///
    /// A `spawn(2)` failure is almost always a configuration problem in one
    /// daemon's spec — typically a missing binary, an unreadable log path,
    /// or an env var that resolves to garbage. None of those are reasons to
    /// take down the whole supervisor: the other daemons are healthy and
    /// the operator may not even notice the broken one for hours.
    ///
    /// We therefore log the error to stderr (so it surfaces in the supervisor's
    /// `StandardErrorPath`) and mark the child as `Exited` with a synthetic
    /// `last_exit_code = Some(127)` (the POSIX convention for "command not
    /// found"). The throttle policy then parks the child for
    /// `spec.throttle_seconds` before the next retry. If the operator fixes
    /// the config the next attempt succeeds; if not, the supervisor keeps
    /// running every other daemon while the broken one stays in a slow
    /// retry loop.
    ///
    /// This is the fix for the silent exit-and-respawn loop:
    /// previously the `?` propagated the error up
    /// through `step_once` → `run` → `main`, which exited with code 5,
    /// launchd respawned the supervisor every 5 s, every healthy child
    /// got SIGKILL'd as a side effect, and the supervisor's own state
    /// file stopped being updated because `step_once` never reached its
    /// final `persist()` call. The method now returns `()` because there
    /// is no caller-recoverable error left after we apply the policy
    /// inline.
    fn spawn_child(&mut self, name: &str, now: DateTime<Utc>) {
        let Some(spec) = self.children.get(name).map(|r| r.spec.clone()) else {
            return;
        };
        match self.process.spawn(&spec) {
            Ok(spawned) => {
                if let Some(rec) = self.children.get_mut(name) {
                    rec.status = ChildStatus::Running;
                    rec.pid = Some(spawned.pid);
                    rec.last_spawn_at = Some(now);
                    rec.throttle_until = None;
                    rec.respawn_count = rec.respawn_count.saturating_add(1);
                }
            }
            Err(e) => {
                eprintln!(
                    "cosmon-daemon-supervisor: spawn '{}' failed: {e} \
                     (will retry after {}s throttle)",
                    spec.name, spec.throttle_seconds
                );
                if let Some(rec) = self.children.get_mut(name) {
                    rec.status = ChildStatus::Exited;
                    rec.pid = None;
                    rec.last_exit_code = Some(127);
                    rec.last_exit_at = Some(now);
                    rec.respawn_count = rec.respawn_count.saturating_add(1);
                }
                // A failed spawn (missing binary, bad log path, garbage env)
                // is exactly the semantically-broken-config crash the
                // escape valve must surface: it will retry-and-fail forever
                // otherwise. Count it like any other crash.
                self.record_crash(name, now);
            }
        }
    }

    /// Record one crash for `name` and, if it tips the rolling window past
    /// [`Self::crash_loop_threshold`], fire a single `PropulsionDown` alert.
    ///
    /// This is the crash-loop escape valve (ADR-053
    /// ~:220). The supervisor's `Exited → throttle → SpawnNow` policy is
    /// correct but silent: a child whose config parses yet is semantically
    /// broken re-spawns forever and *nothing the operator watches ever
    /// fires*. After K crashes inside the window the supervisor emits one
    /// operator-visible alert instead of crash-looping in the dark. The
    /// alert is **latched** (`propulsion_alerted`) so it fires once per
    /// episode, and re-arms when the child recovers (recent count drops
    /// back below the threshold).
    fn record_crash(&mut self, name: &str, now: DateTime<Utc>) {
        let threshold = self.crash_loop_threshold;
        let window = self.crash_loop_window;
        let (should_fire, recent) = {
            let Some(rec) = self.children.get_mut(name) else {
                return;
            };
            rec.crash_times = prune_crash_times(&rec.crash_times, now, window);
            rec.crash_times.push(now);
            let alert = crash_loop_alert(&rec.crash_times, now, threshold, window);
            let recent = rec.crash_times.len();
            let fire = alert && !rec.propulsion_alerted;
            if fire {
                rec.propulsion_alerted = true;
            } else if !alert {
                // Recovered enough to drop below threshold — re-arm.
                rec.propulsion_alerted = false;
            }
            (fire, recent)
        };
        if should_fire {
            self.emit_propulsion_down(name, recent);
        }
    }

    /// Surface a `PropulsionDown` alert on the operator-visible notify
    /// channel by shelling out to [`Self::notify_command`] (default
    /// `cs notify`). **Best-effort**: the decision is always logged to
    /// stderr (which lands in the supervisor's `StandardErrorPath`), and a
    /// missing / failing notify command never propagates — surfacing the
    /// crash loop must not itself crash the supervisor.
    ///
    /// The message is appended after `--title PropulsionDown --level alert`,
    /// matching the `cs notify` CLI surface. An empty `notify_command`
    /// disables dispatch (stderr log only).
    fn emit_propulsion_down(&self, name: &str, recent: usize) {
        let window_s = self.crash_loop_window.num_seconds();
        let msg = format!(
            "PROPULSION DOWN: daemon '{name}' crash-looped {recent} times in {window_s}s. \
             The supervisor keeps restarting it but it will not stay up — inspect its \
             binary/config (a semantically-broken config that parses is the usual cause)."
        );
        // Always log: the stderr trail is the floor, the notify channel the
        // ceiling. Even with no channel configured the operator can grep.
        eprintln!("cosmon-daemon-supervisor: {msg}");

        let Some((program, rest)) = self.notify_command.split_first() else {
            return; // empty notify_command → stderr log only
        };
        let mut cmd = std::process::Command::new(program);
        cmd.args(rest)
            .args(["--title", "PropulsionDown", "--level", "alert", &msg]);
        match cmd.status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("cosmon-daemon-supervisor: PropulsionDown notify exited non-zero ({s})");
            }
            Err(e) => {
                eprintln!("cosmon-daemon-supervisor: PropulsionDown notify spawn failed: {e}");
            }
        }
    }

    /// Apply a hot-reload of the config file: re-parse, diff, spawn/kill/update.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] if the new config cannot be loaded or
    /// any spawn/signal fails.
    pub fn reload(&mut self) -> Result<crate::reload::DiffResult, SupervisorError> {
        let new_cfg = Config::load(&self.config_path)?;
        let old_map: HashMap<String, DaemonSpec> = self
            .children
            .iter()
            .map(|(k, v)| (k.clone(), v.spec.clone()))
            .collect();
        let new_map = new_cfg.by_name();
        let d = diff(&old_map, &new_map);

        // Kill dropped daemons.
        for name in &d.kill {
            if let Some(rec) = self.children.get(name) {
                if let Some(pid) = rec.pid {
                    let _ = self.process.signal(pid, Signal::Term);
                }
            }
            self.children.remove(name);
        }
        // Spawn new daemons (deferred to step_once — just add the record).
        for name in &d.spawn {
            if let Some(spec) = new_map.get(name) {
                self.children
                    .insert(name.clone(), ChildRecord::fresh(spec.clone()));
            }
        }
        // For changed daemons: SIGTERM the current child and replace the spec;
        // next step_once will spawn fresh.
        for name in &d.changed {
            if let Some(rec) = self.children.get_mut(name) {
                if let Some(pid) = rec.pid {
                    let _ = self.process.signal(pid, Signal::Term);
                }
                if let Some(spec) = new_map.get(name) {
                    rec.spec = spec.clone();
                }
                rec.status = ChildStatus::Exited;
                rec.pid = None;
                rec.last_exit_at = Some(Utc::now());
            }
        }
        self.persist()?;
        Ok(d)
    }

    /// Gracefully stop every supervised child: SIGTERM, wait up to
    /// `self.term_grace`, SIGKILL survivors.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] only on state-save failure; signal
    /// delivery failures against already-dead pids are swallowed because
    /// that's the happy path for shutdown.
    pub async fn shutdown(&mut self) -> Result<(), SupervisorError> {
        let pids: Vec<u32> = self.children.values().filter_map(|r| r.pid).collect();
        for pid in &pids {
            let _ = self.process.signal(*pid, Signal::Term);
        }

        let deadline = std::time::Instant::now() + self.term_grace;

        // Phase 1: wait for polite termination or hit the grace deadline.
        loop {
            self.reap_owned_once();
            let any_alive = self
                .children
                .values()
                .any(|r| r.pid.is_some_and(pid_is_alive));
            if !any_alive {
                self.persist()?;
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }

        // Phase 2: escalate to SIGKILL and wait for reaping. SIGKILL
        // cannot be trapped, so any process that's still up after this
        // pass is a zombie waiting to be collected — loop until every
        // child handle has been reaped, or a hard deadline (2 s) hits
        // to prevent indefinite hangs in test harnesses.
        let kill_pids: Vec<u32> = self
            .children
            .values()
            .filter_map(|r| r.pid)
            .filter(|pid| pid_is_alive(*pid))
            .collect();
        for pid in kill_pids {
            let _ = self.process.signal(pid, Signal::Kill);
        }

        let hard_deadline = std::time::Instant::now() + StdDuration::from_secs(2);
        while std::time::Instant::now() < hard_deadline {
            self.reap_owned_once();
            let any_alive = self
                .children
                .values()
                .any(|r| r.pid.is_some_and(pid_is_alive));
            if !any_alive {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }

        self.persist()?;
        Ok(())
    }

    /// One non-blocking reap pass over every owned child. Children we
    /// inherited (no tokio handle) are ignored here — their death is
    /// detected via [`pid_is_alive`] in the shutdown loop.
    fn reap_owned_once(&mut self) {
        let owned: Vec<(String, u32)> = self
            .children
            .iter()
            .filter_map(|(n, r)| r.pid.map(|p| (n.clone(), p)))
            .filter(|(_, pid)| self.process.has_child(*pid))
            .collect();
        for (name, pid) in owned {
            let Ok(outcome) = self.process.reap(pid) else {
                continue;
            };
            let now = Utc::now();
            match outcome {
                crate::ports::ReapOutcome::Alive => {}
                crate::ports::ReapOutcome::Exited(code) => {
                    if let Some(rec) = self.children.get_mut(&name) {
                        rec.status = ChildStatus::Exited;
                        rec.pid = None;
                        rec.last_exit_code = Some(code);
                        rec.last_exit_at = Some(now);
                    }
                }
                crate::ports::ReapOutcome::Signaled => {
                    if let Some(rec) = self.children.get_mut(&name) {
                        rec.status = ChildStatus::Exited;
                        rec.pid = None;
                        rec.last_exit_code = None;
                        rec.last_exit_at = Some(now);
                    }
                }
            }
        }
    }

    /// Snapshot of the current child statuses. Test / diagnostic helper.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(String, ChildStatus, Option<u32>)> {
        let mut v: Vec<_> = self
            .children
            .values()
            .map(|r| (r.name.clone(), r.status, r.pid))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Number of children currently marked `Running`.
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.children
            .values()
            .filter(|r| r.status == ChildStatus::Running)
            .count()
    }
}

/// Drive the supervisor until a SIGTERM is received.
///
/// Composition root of the binary. Exposed as a library function so the
/// integration test `signal_cascade.rs` can exercise the same code path
/// that the `LaunchAgent` will hit in production.
///
/// `tick_interval` is the cadence of the throttle / policy evaluation loop;
/// keeping it at 1 s matches the resolution of `DaemonSpec::throttle_seconds`.
///
/// # Errors
///
/// Propagates any [`SupervisorError`] from the initial state recovery or
/// event-loop iterations.
pub async fn run(
    mut supervisor: Supervisor,
    tick_interval: StdDuration,
) -> Result<(), SupervisorError> {
    // Initial pass — spawn everything that's due.
    supervisor.step_once()?;

    let shutdown_flag = Arc::new(AtomicBool::new(false));

    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| SupervisorError::Io(format!("install SIGTERM handler: {e}")))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| SupervisorError::Io(format!("install SIGINT handler: {e}")))?;
    let mut sigchld = signal(SignalKind::child())
        .map_err(|e| SupervisorError::Io(format!("install SIGCHLD handler: {e}")))?;
    let mut ticker = time::interval(tick_interval);
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

    loop {
        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {
            _ = sigterm.recv() => {
                shutdown_flag.store(true, Ordering::SeqCst);
                break;
            }
            _ = sigint.recv() => {
                shutdown_flag.store(true, Ordering::SeqCst);
                break;
            }
            _ = sigchld.recv() => {
                supervisor.step_once()?;
            }
            maybe = supervisor.config_rx.recv() => {
                if maybe.is_some() {
                    let _ = supervisor.reload();
                    supervisor.step_once()?;
                }
            }
            _ = ticker.tick() => {
                supervisor.step_once()?;
            }
        }
    }

    supervisor.shutdown().await?;
    Ok(())
}
