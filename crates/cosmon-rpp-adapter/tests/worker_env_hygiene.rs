// SPDX-License-Identifier: AGPL-3.0-only

//! §3.5 envelope — the allow-list half, at its U6 home: the worker
//! spawn.
//!
//! The retired subprocess envelope had two halves: **set** the
//! canonical vars, and **clear** the adapter's own environment,
//! re-admitting only the names on
//! [`cosmon_rpp_adapter::worker_env::PASSTHROUGH_VARS`]. The property
//! is unchanged since delib-20260819-cda2 C2 — `COSMON_SKIP_PRE_DONE_HOOK`,
//! the human operator's kill-switch for the blocking `pre_done` gate,
//! crossed the old deny-list perimeter and disarmed the
//! Definition-of-Done of every subsequent harvest — but the boundary
//! moved: the adapter no longer spawns a `cs` child, so the one place
//! the envelope must hold is the env the executor hands to the
//! tmux/claude worker.
//!
//! These tests exercise that exact seam: an
//! [`cosmon_rpp_adapter::worker_env::EnvelopedBackend`] spawning
//! through the SAME `TransportBackend::spawn` call the library
//! executor makes, into a recording port, and inspecting the
//! `/usr/bin/env -i K=V…` rewrite the worker process would receive.
//! The pure-function half (allow-list predicates, the set-half values
//! over an injected parent env) lives in `worker_env`'s unit tests;
//! what this file adds is the spawn-level proof that the compiled env
//! is what actually reaches the transport port — with `-i` in front,
//! so nothing else does.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use cosmon_core::agent::AgentRole;
use cosmon_core::id::{AgentId, WorkerId};
use cosmon_core::injection::InjectionProvenance;
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};
use cosmon_rpp_adapter::worker_env::{
    is_passthrough, EnvelopedBackend, WorkerEnvelope, PASSTHROUGH_VARS,
};

/// Variables the envelope *sets* itself. Everything else in the worker
/// must be allow-listed, or the perimeter leaks.
const ENVELOPE_SET_VARS: &[&str] = &[
    "COSMON_EGRESS_EXPOSED",
    "COSMON_STATE_DIR",
    "COSMON_ARTIFACT_DIR",
];

/// Adapter-side variables that must never be inherited. A sample, not
/// a roster: under an allow-list the guarantee is structural, and this
/// list exists to name the historically load-bearing cases in a
/// failing assertion.
const POISON_VARS: &[&str] = &[
    // Filestore + CLI resolution (T25 Gap 2).
    "COSMON_STATE_DIR",
    "COSMON_FORMULAS_DIR",
    "COSMON_CONFIG",
    "COSMON_CLUSTER_CONFIG",
    "COSMON_GALAXIES_ROOT",
    "COSMON_GALAXY",
    "COSMON_MOL_DIR",
    "COSMON_CONFIG_HOME",
    "COSMON_CLUSTER_ROOT",
    "COSMON_REPO_ROOT",
    // Capability gates and audit instrumentation.
    "COSMON_OPERATOR_GESTURE",
    "COSMON_OPERATOR_GESTURE_ID",
    "COSMON_TOKEN_INSTRUMENTATION_PATH",
    "COSMON_AUTHZ_INSTRUMENTATION_PATH",
    "COSMON_ARTIFACT_DIR",
    // The operator kill-switch of the blocking `pre_done` gate — the
    // variable the deny-list never carried (delib-20260819-cda2, C2).
    "COSMON_SKIP_PRE_DONE_HOOK",
    // Worker pilotage, which a deny-list keyed on `COSMON_*` would not
    // even have had a shape for.
    "CB_DEPTH",
    "COSMON_EGRESS_POLICY",
    // The retired subprocess-envelope marker: present with value `1` on
    // a worker it would trip the §3.5 second lock on the worker's own
    // `cs evolve` / `cs complete`.
    "COSMON_API_REQUEST",
    "COSMON_API_REQUEST_ID",
    // A name nobody has ever written down anywhere in cosmon: the
    // allow-list must block it precisely because it is unknown.
    "COSMON_A_VARIABLE_INVENTED_TOMORROW",
];

fn envelope(root: &Path) -> WorkerEnvelope {
    WorkerEnvelope {
        tenant_root: root.join("a"),
        artifact_dir: Some(root.join("artifacts").join("a").join("task-1")),
        anthropic_api_key: None,
        claude_model: None,
    }
}

/// Compile the envelope over a poisoned parent env and return the env
/// map a spawned worker would receive.
fn worker_env_under_poison(root: &Path) -> BTreeMap<String, String> {
    let parent: Vec<(String, String)> = POISON_VARS
        .iter()
        .map(|k| ((*k).to_owned(), "/leaked/by/adapter".to_owned()))
        .chain([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("HOME".to_owned(), "/home/user".to_owned()),
        ])
        .collect();
    envelope(root).build_env(parent).into_iter().collect()
}

#[test]
fn asymmetry_adapter_cosmon_state_dir_does_not_reach_worker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let map = worker_env_under_poison(tmp.path());

    for key in POISON_VARS {
        // COSMON_STATE_DIR is cleared THEN re-posed to the canonical
        // tenant store (B1 moussage resident). The hygiene invariant it
        // must satisfy is *stronger* than absence: the worker sees the
        // per-tenant path, never the adapter's leaked value.
        if *key == "COSMON_STATE_DIR" {
            let expected = tmp
                .path()
                .join("a")
                .join(".cosmon")
                .join("state")
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                map.get(*key),
                Some(&expected),
                "COSMON_STATE_DIR must be re-posed to the tenant store, \
                 not inherited from the adapter env"
            );
            continue;
        }
        if *key == "COSMON_ARTIFACT_DIR" {
            // Same shape: set-half wins, the leaked value never survives.
            assert_ne!(
                map.get(*key).map(String::as_str),
                Some("/leaked/by/adapter"),
                "COSMON_ARTIFACT_DIR leaked from the adapter env"
            );
            continue;
        }
        assert!(
            !map.contains_key(*key),
            "leaked adapter var into worker env: {key} = {:?}",
            map.get(*key)
        );
    }
}

/// The named non-regression test for delib-20260819-cda2 C2, restated
/// at the worker boundary: the pre_done kill-switch dies at the
/// adapter perimeter — a container has no operator to make that
/// gesture.
#[test]
fn operator_kill_switch_does_not_reach_worker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let map = worker_env_under_poison(tmp.path());
    assert!(
        !map.contains_key("COSMON_SKIP_PRE_DONE_HOOK"),
        "the pre_done kill-switch crossed the adapter perimeter: {:?}",
        map.get("COSMON_SKIP_PRE_DONE_HOOK")
    );
}

/// The structural invariant the allow-list buys, stated once: every
/// variable in the worker env is either allow-listed or set by the
/// envelope itself. A future variable added anywhere in the workspace
/// is closed on arrival.
#[test]
fn only_allow_listed_vars_reach_worker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let map = worker_env_under_poison(tmp.path());
    let unexpected: Vec<&String> = map
        .keys()
        .filter(|k| !is_passthrough(k) && !ENVELOPE_SET_VARS.contains(&k.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "the worker env carries variables that are neither allow-listed \
         nor set by the envelope: {unexpected:?}"
    );
}

/// The other failure mode of an allow-list: too narrow, and the worker
/// cannot find `git`, `sh` or its own configuration.
#[test]
fn process_basics_survive_the_clear() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let map = worker_env_under_poison(tmp.path());
    assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
    assert_eq!(map.get("HOME").map(String::as_str), Some("/home/user"));
}

/// Minimal recording port: keeps the exact `AgentDefinition` handed to
/// `spawn`, so the test can read the command line a worker would run.
#[derive(Clone, Default)]
struct RecordingBackend {
    spawns: Arc<Mutex<Vec<AgentDefinition>>>,
}

impl TransportBackend for RecordingBackend {
    fn spawn(
        &self,
        agent: &AgentDefinition,
        config: &RuntimeConfig,
    ) -> Result<SpawnHandle, TransportError> {
        self.spawns
            .lock()
            .expect("recording mutex")
            .push(agent.clone());
        let id = WorkerId::new(agent.id.as_str())
            .map_err(|e| TransportError::SpawnFailed(e.to_string()))?;
        Ok(SpawnHandle {
            session_name: format!("{}{}", config.session_prefix, id.name()),
            id,
        })
    }

    fn terminate(&self, _id: &WorkerId) -> Result<(), TransportError> {
        Ok(())
    }

    fn is_alive(&self, _id: &WorkerId) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn send_input(&self, _id: &WorkerId, _input: &str) -> Result<(), TransportError> {
        Ok(())
    }

    fn send_input_observed(
        &self,
        _id: &WorkerId,
        _input: &str,
        _provenance: &InjectionProvenance,
    ) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture_output(&self, _id: &WorkerId, _lines: usize) -> Result<String, TransportError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn graceful_exit(
        &self,
        _id: &WorkerId,
        _timeout: std::time::Duration,
    ) -> Result<bool, TransportError> {
        Ok(true)
    }
}

/// Spawn-level proof: the compiled envelope is what actually crosses
/// the transport port — as a `/usr/bin/env -i K=V…` rewrite of the
/// agent command, so the tmux server's environment (the adapter's) can
/// never reach the worker process. This is the seam the library
/// executor spawns through; stub the decorator out of the route and
/// this test goes red.
#[test]
fn spawn_crosses_the_port_behind_env_i() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let recorder = RecordingBackend::default();
    let enveloped = EnvelopedBackend::new(recorder.clone(), &envelope(tmp.path()));

    let agent = AgentDefinition {
        id: AgentId::new("cosmon-task-20260905-hyg1").expect("valid id"),
        role: AgentRole::Implementation,
        command: "claude".to_owned(),
        args: vec!["--dangerously-skip-permissions".to_owned()],
        cwd: Some(tmp.path().join("worktree")),
    };
    enveloped
        .spawn(&agent, &RuntimeConfig::default())
        .expect("recording spawn succeeds");

    let spawns = recorder.spawns.lock().expect("recording mutex");
    let spawned = spawns.first().expect("one spawn recorded");

    assert_eq!(
        spawned.command, "/usr/bin/env",
        "spawn must go through env(1)"
    );
    assert_eq!(
        spawned.args.first().map(String::as_str),
        Some("-i"),
        "-i must come first so nothing but the compiled set survives"
    );
    // The original command + args survive at the tail, after the pairs.
    // The decorator rewrites the command, never the working directory: the
    // ADR-079 §5 obligation-3 cwd must reach the inner backend intact.
    assert_eq!(
        spawned.cwd.as_deref(),
        Some(tmp.path().join("worktree").as_path()),
        "the envelope must carry the worker cwd through unchanged"
    );
    let n = spawned.args.len();
    assert_eq!(spawned.args[n - 2], "claude");
    assert_eq!(spawned.args[n - 1], "--dangerously-skip-permissions");
    // Every assignment between `-i` and the command is either
    // allow-listed or set-half; nothing else crosses.
    let pairs = &spawned.args[1..n - 2];
    for pair in pairs {
        let (k, _) = pair
            .split_once('=')
            .expect("everything between -i and the command is an assignment");
        assert!(
            is_passthrough(k) || ENVELOPE_SET_VARS.contains(&k),
            "unexpected assignment reached the spawn: {pair}"
        );
    }
    // The exposed-posture stamp (ADR-155 re-homing) rides every spawn.
    assert!(
        pairs
            .iter()
            .any(|p| p.as_str() == "COSMON_EGRESS_EXPOSED=1"),
        "COSMON_EGRESS_EXPOSED=1 missing from the worker spawn env"
    );
    // The retired marker must NOT ride it (lock 2 would refuse the
    // worker's own lifecycle verbs).
    assert!(
        !pairs.iter().any(|p| p.starts_with("COSMON_API_REQUEST")),
        "the retired subprocess marker leaked onto the worker spawn"
    );
}

/// The allow-list itself stays free of `COSMON_*` names — the set half
/// is the only door for those, each with a reason.
#[test]
fn allow_list_carries_no_cosmon_name() {
    for name in PASSTHROUGH_VARS {
        assert!(!name.starts_with("COSMON_"), "{name}");
    }
}
