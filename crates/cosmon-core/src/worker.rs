// SPDX-License-Identifier: AGPL-3.0-only

//! Worker types and lifecycle state.
//!
//! A [`Worker`] is a running instance of an [`AgentDefinition`](crate::agent::AgentDefinition),
//! bound to a specific runtime environment. Workers are ephemeral — they are created,
//! execute work, and stop. Multiple Workers may instantiate the same Agent Definition.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::agent::{AgentDepth, AgentRole, DepthExceeded, ParseEnumError};
use crate::id::{AgentId, MoleculeId, SessionId, WorkerId};

// ---------------------------------------------------------------------------
// WorkerRole — 1-bit discriminator for runtime vs cognition workers
// ---------------------------------------------------------------------------

/// One-bit discriminator distinguishing infrastructure workers from
/// cognitive workers sharing the same molecule id.
///
/// A macro-molecule tackled by `cs tackle` spawns two registered workers:
/// a [`WorkerRole::Runtime`] (the `cs run` polling loop) and, once the root
/// unblocks, a [`WorkerRole::Cognition`] (the Claude Code session actually
/// doing the work). Before this field existed, the pair collided in every
/// downstream surface — `cs peek` displayed the molecule twice, `cs purge`
/// could not filter, and `cs patrol` could not distinguish a legitimate pair
/// from a duplicated worker.
///
/// Derivation order at load time (see [`derive_worker_role`]):
///
/// 1. Persisted field (new writes).
/// 2. [`AgentRole::Runtime`] → [`WorkerRole::Runtime`].
/// 3. Worker-id prefix `runtime-` → [`WorkerRole::Runtime`].
/// 4. Everything else → [`WorkerRole::Cognition`].
///
/// The role is a single bit derived deterministically from the points
/// above — no extra state to keep in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    /// The worker is a cognition process (Claude Code, Codex, etc.)
    /// executing the molecule's formula steps. Default for any legacy
    /// entry without a recorded role.
    Cognition,
    /// The worker is a resident runtime (`cs run`) driving a DAG of
    /// downstream molecules. Infrastructure, not cognition.
    Runtime,
}

impl WorkerRole {
    /// Glyph used by operator-facing renderers to mark a row's role.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Cognition => "🧠",
            Self::Runtime => "🎛️",
        }
    }
}

impl fmt::Display for WorkerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cognition => f.write_str("cognition"),
            Self::Runtime => f.write_str("runtime"),
        }
    }
}

impl FromStr for WorkerRole {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cognition" => Ok(Self::Cognition),
            "runtime" => Ok(Self::Runtime),
            _ => Err(ParseEnumError {
                type_name: "WorkerRole",
                value: s.to_owned(),
            }),
        }
    }
}

/// Derive a [`WorkerRole`] from an [`AgentRole`] + worker id prefix pair.
///
/// Used when a legacy `fleet.json` entry is missing the explicit
/// `worker_role` field. Runtime workers are registered with either
/// `AgentRole::Runtime` or a `runtime-…` name prefix; every other
/// combination is taken to be a cognition worker.
#[must_use]
pub fn derive_worker_role(agent_role: AgentRole, worker_id: &str) -> WorkerRole {
    if agent_role == AgentRole::Runtime || worker_id.starts_with("runtime-") {
        WorkerRole::Runtime
    } else {
        WorkerRole::Cognition
    }
}

/// Lifecycle state of a worker agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// Worker is initialising.
    Starting,
    /// Worker is running and processing work.
    Active,
    /// Worker execution is temporarily suspended.
    Paused,
    /// Worker is in the process of shutting down.
    Stopping,
    /// Worker has terminated normally.
    Stopped,
    /// Worker encountered an error with the given message.
    Error(String),
    /// Worker failed a single liveness check — may be slow, not dead yet.
    /// If still unresponsive on the next patrol, transitions to `Stale`.
    /// If alive on next check, recovers to `Active`.
    Unresponsive,
    /// Worker is confirmed unresponsive (failed two consecutive checks) and presumed dead.
    Stale,
}

impl fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => f.write_str("starting"),
            Self::Active => f.write_str("active"),
            Self::Paused => f.write_str("paused"),
            Self::Stopping => f.write_str("stopping"),
            Self::Stopped => f.write_str("stopped"),
            Self::Error(msg) => write!(f, "error:{msg}"),
            Self::Unresponsive => f.write_str("unresponsive"),
            Self::Stale => f.write_str("stale"),
        }
    }
}

impl FromStr for WorkerStatus {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "starting" => Ok(Self::Starting),
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "stopping" => Ok(Self::Stopping),
            "stopped" => Ok(Self::Stopped),
            "unresponsive" => Ok(Self::Unresponsive),
            "stale" => Ok(Self::Stale),
            _ if s.starts_with("error:") => Ok(Self::Error(s[6..].to_owned())),
            _ => Err(ParseEnumError {
                type_name: "WorkerStatus",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Desired / Observed / Effective — the reconciliation model
// ---------------------------------------------------------------------------

/// What the operator wants this worker to be doing.
///
/// Persisted in fleet.json — the single source of intent. Unlike
/// [`WorkerStatus`] (which conflates intent, observation, and health),
/// `DesiredState` captures *only* the operator's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    /// Worker should be active and processing work.
    Running,
    /// Worker should be frozen (preempted or manually paused).
    Paused,
    /// Worker should not exist.
    Stopped,
}

impl fmt::Display for DesiredState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Paused => f.write_str("paused"),
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

impl FromStr for DesiredState {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "stopped" => Ok(Self::Stopped),
            _ => Err(ParseEnumError {
                type_name: "DesiredState",
                value: s.to_owned(),
            }),
        }
    }
}

/// Transport-layer liveness — is the process alive?
///
/// Derived fresh on every observation, never persisted. Maps directly
/// to the result of [`TransportBackend::is_alive`](crate::transport::TransportBackend::is_alive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportState {
    /// Process is alive (tmux session exists).
    Alive,
    /// Process is dead (no session).
    Dead,
    /// Cannot determine (backend unavailable or I/O error).
    Unknown,
}

impl fmt::Display for TransportState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Alive => f.write_str("alive"),
            Self::Dead => f.write_str("dead"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// Agent self-declared cognitive status.
///
/// Read from cognitive status files (e.g. `state/cognitive/{worker}.json`).
/// Derived fresh, never persisted in fleet.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CognitiveState {
    /// Agent recently declared its status (within freshness window).
    Fresh(String),
    /// Agent's declaration is stale (older than freshness threshold).
    Stale,
    /// No cognitive status file exists for this worker.
    None,
}

impl fmt::Display for CognitiveState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh(s) => write!(f, "fresh:{s}"),
            Self::Stale => f.write_str("stale"),
            Self::None => f.write_str("none"),
        }
    }
}

/// What reality says about a worker — computed fresh, never persisted.
///
/// Combines three orthogonal observations: transport liveness, session
/// activity (as a string to avoid depending on `cosmon-transport`), and
/// agent self-declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedState {
    /// Is the transport (tmux) session alive?
    pub transport: TransportState,
    /// What is the session doing? Stringified from `SessionStatus`.
    /// `None` when transport is Dead or Unknown.
    pub session: Option<String>,
    /// Agent self-declared cognitive state.
    pub cognitive: CognitiveState,
}

/// What the user sees — computed by [`reconcile`], displayed in ensemble.
///
/// This replaces the role of [`WorkerStatus`] for user-facing display.
/// It is never persisted — always recomputed from desired + observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStatus {
    /// Desired=Running, transport alive, agent responsive.
    Healthy,
    /// Desired vs. reality mismatch (dead process, zombie, etc.).
    Diverged,
    /// Desired=Running, alive but something looks off (stale cognitive, unknown session).
    Suspect,
    /// Session is blocked on a prompt (permission dialog, trust prompt).
    Blocked,
    /// Desired=Stopped and process is gone.
    Stopped,
    /// Desired=Paused.
    Paused,
    /// Unrecoverable error (circuit-breaker tripped, etc.).
    Error(String),
}

impl fmt::Display for EffectiveStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Diverged => f.write_str("diverged"),
            Self::Suspect => f.write_str("suspect"),
            Self::Blocked => f.write_str("blocked"),
            Self::Stopped => f.write_str("stopped"),
            Self::Paused => f.write_str("paused"),
            Self::Error(msg) => write!(f, "error:{msg}"),
        }
    }
}

/// Action the system should take to reconcile desired with observed.
///
/// Returned by [`reconcile`] alongside [`EffectiveStatus`]. The caller
/// is responsible for executing these actions (spawn tmux, kill session, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// No action needed — state matches intent.
    Noop,
    /// Spawn a new process for this worker.
    Spawn,
    /// Kill the zombie process (desired=Stopped but process alive).
    Kill,
    /// Freeze the running process (desired=Paused but process running).
    Freeze,
    /// Thaw the frozen process (desired=Running but was paused).
    Thaw,
    /// Record a liveness failure (increment counter).
    RecordFailure,
    /// Attempt respawn (dead process, under restart limit).
    Respawn,
    /// Circuit-break — stop trying (restart limit exceeded).
    CircuitBreak,
}

/// Pure reconciliation — the single source of truth for state decisions.
///
/// Given what the operator wants (`desired`) and what reality shows
/// (`observed`), computes what the user should see ([`EffectiveStatus`])
/// and what actions the system should take ([`ReconcileAction`]s).
///
/// This function is pure (no I/O, no side effects). Every CLI command
/// should call it instead of ad-hoc status checks.
///
/// # Arguments
///
/// * `desired` — operator intent (persisted in fleet.json, `Copy`)
/// * `observed` — fresh observation (transport + session + cognitive)
/// * `consecutive_failures` — how many liveness checks have failed in a row
/// * `max_restarts` — circuit-breaker threshold
#[must_use]
pub fn reconcile(
    desired: DesiredState,
    observed: &ObservedState,
    consecutive_failures: u32,
    max_restarts: u32,
) -> (EffectiveStatus, Vec<ReconcileAction>) {
    match (desired, observed.transport) {
        // ── Desired=Running ──────────────────────────────────────
        (DesiredState::Running, TransportState::Alive) => reconcile_running_alive(observed),
        (DesiredState::Running, TransportState::Dead) => {
            if consecutive_failures >= max_restarts {
                (
                    EffectiveStatus::Error("restart limit exceeded".to_owned()),
                    vec![ReconcileAction::CircuitBreak],
                )
            } else {
                (EffectiveStatus::Diverged, vec![ReconcileAction::Respawn])
            }
        }
        (DesiredState::Running, TransportState::Unknown) => {
            // Can't determine liveness — flag as suspect, wait for next check.
            (EffectiveStatus::Suspect, vec![ReconcileAction::Noop])
        }

        // ── Desired=Paused ───────────────────────────────────────
        (DesiredState::Paused, TransportState::Alive) => {
            // Process still alive but should be frozen — need to freeze it.
            (EffectiveStatus::Paused, vec![ReconcileAction::Freeze])
        }
        (DesiredState::Paused, TransportState::Dead | TransportState::Unknown) => {
            // Paused and no process — that's fine.
            (EffectiveStatus::Paused, vec![ReconcileAction::Noop])
        }

        // ── Desired=Stopped ──────────────────────────────────────
        (DesiredState::Stopped, TransportState::Alive) => {
            // Zombie — process exists but shouldn't.
            (EffectiveStatus::Diverged, vec![ReconcileAction::Kill])
        }
        (DesiredState::Stopped, TransportState::Dead | TransportState::Unknown) => {
            // Clean stop.
            (EffectiveStatus::Stopped, vec![ReconcileAction::Noop])
        }
    }
}

/// Refine the Running+Alive case using session and cognitive details.
fn reconcile_running_alive(observed: &ObservedState) -> (EffectiveStatus, Vec<ReconcileAction>) {
    // Check for blocked session first — most urgent.
    if let Some(ref session) = observed.session {
        if session == "blocked" || session == "trust-prompt" {
            return (EffectiveStatus::Blocked, vec![ReconcileAction::Noop]);
        }
    }

    // Check cognitive state.
    match &observed.cognitive {
        CognitiveState::Fresh(_) | CognitiveState::None => {
            (EffectiveStatus::Healthy, vec![ReconcileAction::Noop])
        }
        CognitiveState::Stale => {
            // Agent is alive but cognitive declaration is stale — suspect.
            (
                EffectiveStatus::Suspect,
                vec![ReconcileAction::RecordFailure],
            )
        }
    }
}

/// A running instance of an Agent Definition, bound to a runtime environment.
///
/// Workers are ephemeral process containers. They reference an
/// [`AgentDefinition`](crate::agent::AgentDefinition) by name and track their
/// lifecycle state, session, and current work assignment.
///
/// JSON serialization is implemented manually to support schema evolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    /// Unique identifier for this worker instance.
    pub id: WorkerId,
    /// Name of the agent definition this worker instantiates.
    pub definition: AgentId,
    /// Current lifecycle state.
    pub status: WorkerStatus,
    /// Session this worker is running in, if any.
    pub session: Option<SessionId>,
    /// When this worker was started.
    pub started_at: DateTime<Utc>,
    /// Molecule the worker is currently executing, if any.
    pub current_molecule: Option<MoleculeId>,
    /// Nesting depth of this worker in the spawn tree.
    ///
    /// Root workers have depth 0. Each agent-spawns-agent increments by one.
    /// Enforced at construction time via [`AgentDepth`].
    pub depth: AgentDepth,
}

impl Worker {
    /// Create a new root worker (depth 0) in the Starting state.
    #[must_use]
    pub fn new(id: WorkerId, definition: AgentId, started_at: DateTime<Utc>) -> Self {
        Self {
            id,
            definition,
            status: WorkerStatus::Starting,
            session: None,
            started_at,
            current_molecule: None,
            depth: AgentDepth::root(),
        }
    }

    /// Create a child worker spawned by a parent at the given depth.
    ///
    /// The child's depth is `parent_depth + 1`.
    ///
    /// # Errors
    /// Returns [`crate::agent::DepthExceeded`] if the parent is already at
    /// [`MAX_AGENT_DEPTH`](crate::agent::MAX_AGENT_DEPTH).
    pub fn spawn_child(
        id: WorkerId,
        definition: AgentId,
        started_at: DateTime<Utc>,
        parent_depth: AgentDepth,
    ) -> Result<Self, crate::agent::DepthExceeded> {
        let depth = parent_depth.spawn_child()?;
        Ok(Self {
            id,
            definition,
            status: WorkerStatus::Starting,
            session: None,
            started_at,
            current_molecule: None,
            depth,
        })
    }
}

impl Serialize for Worker {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", self.id.as_str())?;
        map.serialize_entry("definition", self.definition.as_str())?;
        map.serialize_entry("status", &self.status.to_string())?;
        if let Some(ref session) = self.session {
            map.serialize_entry("session", session.as_str())?;
        }
        map.serialize_entry("started_at", &self.started_at.to_rfc3339())?;
        if let Some(ref mol) = self.current_molecule {
            map.serialize_entry("current_molecule", mol.as_str())?;
        }
        map.serialize_entry("depth", &self.depth.value())?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Worker {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = v
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("Worker must be a JSON object"))?;

        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("id"))
            .and_then(|s| WorkerId::new(s).map_err(serde::de::Error::custom))?;

        let definition = obj
            .get("definition")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("definition"))
            .and_then(|s| AgentId::new(s).map_err(serde::de::Error::custom))?;

        let status: WorkerStatus = obj
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("status"))
            .and_then(|s| s.parse().map_err(serde::de::Error::custom))?;

        let session = obj
            .get("session")
            .and_then(|v| v.as_str())
            .map(SessionId::new)
            .transpose()
            .map_err(serde::de::Error::custom)?;

        let started_at: DateTime<Utc> = obj
            .get("started_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("started_at"))
            .and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(serde::de::Error::custom)
            })?;

        let current_molecule = obj
            .get("current_molecule")
            .and_then(|v| v.as_str())
            .map(MoleculeId::new)
            .transpose()
            .map_err(serde::de::Error::custom)?;

        let depth = obj
            .get("depth")
            .and_then(serde_json::Value::as_u64)
            .map(|v| {
                let v = u32::try_from(v).map_err(|_| DepthExceeded { depth: u32::MAX })?;
                AgentDepth::new(v)
            })
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or_else(AgentDepth::root);

        Ok(Self {
            id,
            definition,
            status,
            session,
            started_at,
            current_molecule,
            depth,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_status_display_roundtrip() {
        let statuses = [
            WorkerStatus::Starting,
            WorkerStatus::Active,
            WorkerStatus::Paused,
            WorkerStatus::Stopping,
            WorkerStatus::Stopped,
            WorkerStatus::Error("connection lost".to_owned()),
            WorkerStatus::Stale,
        ];
        for status in statuses {
            let s = status.to_string();
            let parsed: WorkerStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_worker_json_roundtrip() {
        let worker = Worker {
            id: WorkerId::new("ep-quartz").unwrap(),
            definition: AgentId::new("witness").unwrap(),
            status: WorkerStatus::Active,
            session: Some(SessionId::new("sess-001").unwrap()),
            started_at: chrono::Utc::now(),
            current_molecule: Some(MoleculeId::new("cs-20260401-abcd").unwrap()),
            depth: AgentDepth::new(2).unwrap(),
        };
        let json = serde_json::to_string_pretty(&worker).unwrap();
        let back: Worker = serde_json::from_str(&json).unwrap();
        assert_eq!(worker.id, back.id);
        assert_eq!(worker.definition, back.definition);
        assert_eq!(worker.status, back.status);
        assert_eq!(worker.session, back.session);
        assert_eq!(worker.current_molecule, back.current_molecule);
    }

    #[test]
    fn test_worker_json_optional_fields_absent() {
        let json = r#"{
            "id": "quartz",
            "definition": "polecat",
            "status": "starting",
            "started_at": "2026-04-01T12:00:00Z"
        }"#;
        let worker: Worker = serde_json::from_str(json).unwrap();
        assert!(worker.session.is_none());
        assert!(worker.current_molecule.is_none());
    }

    #[test]
    fn test_worker_json_ignores_unknown_fields() {
        let json = r#"{
            "id": "quartz",
            "definition": "polecat",
            "status": "active",
            "started_at": "2026-04-01T12:00:00Z",
            "unknown_field": 42
        }"#;
        let worker: Worker = serde_json::from_str(json).unwrap();
        assert_eq!(worker.status, WorkerStatus::Active);
    }

    #[test]
    fn test_worker_new_defaults() {
        let w = Worker::new(
            WorkerId::new("jasper").unwrap(),
            AgentId::new("polecat").unwrap(),
            chrono::Utc::now(),
        );
        assert_eq!(w.status, WorkerStatus::Starting);
        assert!(w.session.is_none());
        assert!(w.current_molecule.is_none());
        assert_eq!(w.depth.value(), 0);
    }

    #[test]
    fn test_worker_spawn_child() {
        let parent = Worker::new(
            WorkerId::new("mayor").unwrap(),
            AgentId::new("orchestrator").unwrap(),
            chrono::Utc::now(),
        );
        let child = Worker::spawn_child(
            WorkerId::new("jasper").unwrap(),
            AgentId::new("polecat").unwrap(),
            chrono::Utc::now(),
            parent.depth,
        )
        .unwrap();
        assert_eq!(child.depth.value(), 1);
    }

    #[test]
    fn test_worker_spawn_child_at_max_depth_fails() {
        let max_depth = AgentDepth::new(crate::agent::MAX_AGENT_DEPTH).unwrap();
        let result = Worker::spawn_child(
            WorkerId::new("too-deep").unwrap(),
            AgentId::new("polecat").unwrap(),
            chrono::Utc::now(),
            max_depth,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_worker_json_depth_roundtrip() {
        let parent_depth = AgentDepth::new(2).unwrap();
        let worker = Worker::spawn_child(
            WorkerId::new("quartz").unwrap(),
            AgentId::new("polecat").unwrap(),
            chrono::Utc::now(),
            parent_depth,
        )
        .unwrap();
        assert_eq!(worker.depth.value(), 3);

        let json = serde_json::to_string_pretty(&worker).unwrap();
        let back: Worker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.depth, worker.depth);
    }

    #[test]
    fn test_worker_json_missing_depth_defaults_to_root() {
        let json = r#"{
            "id": "quartz",
            "definition": "polecat",
            "status": "starting",
            "started_at": "2026-04-01T12:00:00Z"
        }"#;
        let worker: Worker = serde_json::from_str(json).unwrap();
        assert_eq!(worker.depth.value(), 0);
    }

    // ── DesiredState tests ───────────────────────────────────

    #[test]
    fn test_desired_state_display_roundtrip() {
        for state in [
            DesiredState::Running,
            DesiredState::Paused,
            DesiredState::Stopped,
        ] {
            let s = state.to_string();
            let parsed: DesiredState = s.parse().unwrap();
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn test_desired_state_parse_invalid() {
        assert!("active".parse::<DesiredState>().is_err());
        assert!("starting".parse::<DesiredState>().is_err());
    }

    #[test]
    fn test_desired_state_serde_roundtrip() {
        for state in [
            DesiredState::Running,
            DesiredState::Paused,
            DesiredState::Stopped,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: DesiredState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
    }

    // ── reconcile() tests ────────────────────────────────────

    /// Helper: build an `ObservedState` quickly.
    fn obs(
        transport: TransportState,
        session: Option<&str>,
        cognitive: CognitiveState,
    ) -> ObservedState {
        ObservedState {
            transport,
            session: session.map(String::from),
            cognitive,
        }
    }

    // ── Running × Alive ──

    #[test]
    fn test_reconcile_running_alive_fresh() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(
                TransportState::Alive,
                Some("idle"),
                CognitiveState::Fresh("working".to_owned()),
            ),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Healthy);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    #[test]
    fn test_reconcile_running_alive_no_cognitive() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Alive, Some("working"), CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Healthy);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    #[test]
    fn test_reconcile_running_alive_stale_cognitive() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Alive, Some("idle"), CognitiveState::Stale),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Suspect);
        assert_eq!(actions, vec![ReconcileAction::RecordFailure]);
    }

    #[test]
    fn test_reconcile_running_alive_blocked_session() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Alive, Some("blocked"), CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Blocked);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    #[test]
    fn test_reconcile_running_alive_trust_prompt() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(
                TransportState::Alive,
                Some("trust-prompt"),
                CognitiveState::None,
            ),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Blocked);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Running × Dead ──

    #[test]
    fn test_reconcile_running_dead_under_limit() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            1,
            3,
        );
        assert_eq!(status, EffectiveStatus::Diverged);
        assert_eq!(actions, vec![ReconcileAction::Respawn]);
    }

    #[test]
    fn test_reconcile_running_dead_at_limit() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            3,
            3,
        );
        assert_eq!(
            status,
            EffectiveStatus::Error("restart limit exceeded".to_owned())
        );
        assert_eq!(actions, vec![ReconcileAction::CircuitBreak]);
    }

    #[test]
    fn test_reconcile_running_dead_over_limit() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            5,
            3,
        );
        assert_eq!(
            status,
            EffectiveStatus::Error("restart limit exceeded".to_owned())
        );
        assert_eq!(actions, vec![ReconcileAction::CircuitBreak]);
    }

    // ── Running × Unknown ──

    #[test]
    fn test_reconcile_running_unknown() {
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Unknown, None, CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Suspect);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Paused × Alive ──

    #[test]
    fn test_reconcile_paused_alive() {
        let (status, actions) = reconcile(
            DesiredState::Paused,
            &obs(TransportState::Alive, Some("idle"), CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Paused);
        assert_eq!(actions, vec![ReconcileAction::Freeze]);
    }

    // ── Paused × Dead ──

    #[test]
    fn test_reconcile_paused_dead() {
        let (status, actions) = reconcile(
            DesiredState::Paused,
            &obs(TransportState::Dead, None, CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Paused);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Paused × Unknown ──

    #[test]
    fn test_reconcile_paused_unknown() {
        let (status, actions) = reconcile(
            DesiredState::Paused,
            &obs(TransportState::Unknown, None, CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Paused);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Stopped × Alive (zombie) ──

    #[test]
    fn test_reconcile_stopped_alive_zombie() {
        let (status, actions) = reconcile(
            DesiredState::Stopped,
            &obs(TransportState::Alive, Some("idle"), CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Diverged);
        assert_eq!(actions, vec![ReconcileAction::Kill]);
    }

    // ── Stopped × Dead (clean) ──

    #[test]
    fn test_reconcile_stopped_dead() {
        let (status, actions) = reconcile(
            DesiredState::Stopped,
            &obs(TransportState::Dead, None, CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Stopped);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Stopped × Unknown ──

    #[test]
    fn test_reconcile_stopped_unknown() {
        let (status, actions) = reconcile(
            DesiredState::Stopped,
            &obs(TransportState::Unknown, None, CognitiveState::None),
            0,
            3,
        );
        assert_eq!(status, EffectiveStatus::Stopped);
        assert_eq!(actions, vec![ReconcileAction::Noop]);
    }

    // ── Purity check ──

    #[test]
    fn test_reconcile_is_pure() {
        let desired = DesiredState::Running;
        let observed = obs(
            TransportState::Alive,
            Some("working"),
            CognitiveState::Fresh("ok".to_owned()),
        );
        let (s1, a1) = reconcile(desired, &observed, 0, 3);
        let (s2, a2) = reconcile(desired, &observed, 0, 3);
        assert_eq!(s1, s2);
        assert_eq!(a1, a2);
    }

    // ── Circuit-breaker boundary ──

    #[test]
    fn test_reconcile_circuit_breaker_boundary() {
        // At max_restarts - 1: still respawn.
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            2,
            3,
        );
        assert_eq!(status, EffectiveStatus::Diverged);
        assert_eq!(actions, vec![ReconcileAction::Respawn]);

        // At max_restarts: circuit-break.
        let (status, actions) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            3,
            3,
        );
        assert_eq!(
            status,
            EffectiveStatus::Error("restart limit exceeded".to_owned())
        );
        assert_eq!(actions, vec![ReconcileAction::CircuitBreak]);
    }

    // ── WorkerRole tests ──

    #[test]
    fn test_worker_role_display_roundtrip() {
        for r in [WorkerRole::Cognition, WorkerRole::Runtime] {
            let s = r.to_string();
            let parsed: WorkerRole = s.parse().unwrap();
            assert_eq!(parsed, r);
        }
    }

    #[test]
    fn test_worker_role_serde_roundtrip() {
        for r in [WorkerRole::Cognition, WorkerRole::Runtime] {
            let json = serde_json::to_string(&r).unwrap();
            let back: WorkerRole = serde_json::from_str(&json).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn test_worker_role_glyph_distinct() {
        assert_ne!(
            WorkerRole::Cognition.glyph(),
            WorkerRole::Runtime.glyph(),
            "cognition and runtime glyphs must be distinct"
        );
    }

    #[test]
    fn test_derive_worker_role_from_agent_role_runtime() {
        assert_eq!(
            derive_worker_role(AgentRole::Runtime, "jasper-xyz"),
            WorkerRole::Runtime
        );
    }

    #[test]
    fn test_derive_worker_role_from_name_prefix() {
        assert_eq!(
            derive_worker_role(AgentRole::Implementation, "runtime-foo-abcd"),
            WorkerRole::Runtime
        );
    }

    #[test]
    fn test_derive_worker_role_defaults_to_cognition() {
        assert_eq!(
            derive_worker_role(AgentRole::Implementation, "quartz-abcd"),
            WorkerRole::Cognition
        );
    }

    #[test]
    fn test_derive_worker_role_cognition_never_misclassified() {
        // Name prefix dominates only when name starts with "runtime-".
        // A random name containing "runtime" mid-string must remain Cognition.
        assert_eq!(
            derive_worker_role(AgentRole::Implementation, "my-runtime-agent"),
            WorkerRole::Cognition
        );
    }

    // ── Every ReconcileAction variant is produced ──

    #[test]
    fn test_all_reconcile_actions_reachable() {
        // Noop
        let (_, a) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Alive, None, CognitiveState::None),
            0,
            3,
        );
        assert!(a.contains(&ReconcileAction::Noop));

        // Respawn
        let (_, a) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            0,
            3,
        );
        assert!(a.contains(&ReconcileAction::Respawn));

        // CircuitBreak
        let (_, a) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Dead, None, CognitiveState::None),
            3,
            3,
        );
        assert!(a.contains(&ReconcileAction::CircuitBreak));

        // Kill
        let (_, a) = reconcile(
            DesiredState::Stopped,
            &obs(TransportState::Alive, None, CognitiveState::None),
            0,
            3,
        );
        assert!(a.contains(&ReconcileAction::Kill));

        // Freeze
        let (_, a) = reconcile(
            DesiredState::Paused,
            &obs(TransportState::Alive, None, CognitiveState::None),
            0,
            3,
        );
        assert!(a.contains(&ReconcileAction::Freeze));

        // RecordFailure
        let (_, a) = reconcile(
            DesiredState::Running,
            &obs(TransportState::Alive, None, CognitiveState::Stale),
            0,
            3,
        );
        assert!(a.contains(&ReconcileAction::RecordFailure));

        // Spawn is not yet produced — will be added when deploy migrates.
        // Thaw is not yet produced — will be added when thaw migrates.
    }
}
