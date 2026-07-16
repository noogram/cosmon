// SPDX-License-Identifier: AGPL-3.0-only

//! Mock adapters driving the four port traits deterministically.
//!
//! Task 1 ships *only* the core + ports; the real adapters land in Task 2.
//! These mocks exist so the event loop (Task 2) can be smoke-tested without
//! a `tokio::process::Command`, `notify::RecommendedWatcher`, or
//! `tempfile::persist()` — all of which pull in either real I/O or a tokio
//! runtime.
//!
//! The goal here is not to *exercise* the real behavior (that's integration
//! tests in Task 2). It is to prove that the port traits are *implementable*
//! without unsafe, that `Box<dyn Port>` works (no missing `Sized` bounds,
//! etc.), and that the supervisor state document round-trips.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, TimeZone, Utc};
use cosmon_daemon_supervisor::config::DaemonSpec;
use cosmon_daemon_supervisor::model::{Child, ChildStatus, Spawning};
use cosmon_daemon_supervisor::ports::{
    ClockPort, ConfigChange, ConfigWatchError, ConfigWatchPort, PersistedChild, ProcessError,
    ProcessPort, ReapOutcome, Signal, SpawnedChild, StateError, StatePort, SupervisorState,
};

// ---------------------------------------------------------------------------
// MockProcessPort
// ---------------------------------------------------------------------------

/// A process port that pretends to spawn: each call just returns a
/// monotonically-increasing pid.
struct MockProcessPort {
    next_pid: u32,
    signaled: Vec<(u32, Signal)>,
    exit_codes: HashMap<u32, Option<i32>>,
}

impl MockProcessPort {
    fn new() -> Self {
        Self {
            next_pid: 1000,
            signaled: Vec::new(),
            exit_codes: HashMap::new(),
        }
    }

    fn schedule_exit(&mut self, pid: u32, code: Option<i32>) {
        self.exit_codes.insert(pid, code);
    }
}

impl ProcessPort for MockProcessPort {
    fn spawn(&mut self, _spec: &DaemonSpec) -> Result<SpawnedChild, ProcessError> {
        let pid = self.next_pid;
        self.next_pid += 1;
        Ok(SpawnedChild {
            pid,
            started_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        })
    }

    fn signal(&mut self, pid: u32, signal: Signal) -> Result<(), ProcessError> {
        self.signaled.push((pid, signal));
        Ok(())
    }

    fn reap(&mut self, pid: u32) -> Result<ReapOutcome, ProcessError> {
        match self.exit_codes.remove(&pid) {
            None => Ok(ReapOutcome::Alive),
            Some(Some(code)) => Ok(ReapOutcome::Exited(code)),
            Some(None) => Ok(ReapOutcome::Signaled),
        }
    }
}

// ---------------------------------------------------------------------------
// MockConfigWatchPort
// ---------------------------------------------------------------------------

struct MockConfigWatchPort {
    queue: VecDeque<ConfigChange>,
}

impl MockConfigWatchPort {
    fn new(changes: impl IntoIterator<Item = DateTime<Utc>>) -> Self {
        Self {
            queue: changes.into_iter().map(|at| ConfigChange { at }).collect(),
        }
    }
}

impl ConfigWatchPort for MockConfigWatchPort {
    fn next(&mut self) -> Result<Option<ConfigChange>, ConfigWatchError> {
        Ok(self.queue.pop_front())
    }
}

// ---------------------------------------------------------------------------
// MockClock
// ---------------------------------------------------------------------------

struct MockClock {
    now: RefCell<DateTime<Utc>>,
}

impl MockClock {
    fn new(at: DateTime<Utc>) -> Self {
        Self {
            now: RefCell::new(at),
        }
    }

    fn advance_secs(&self, s: i64) {
        let current = *self.now.borrow();
        *self.now.borrow_mut() = current + chrono::Duration::try_seconds(s).unwrap();
    }
}

impl ClockPort for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.borrow()
    }
}

// ---------------------------------------------------------------------------
// MockStatePort
// ---------------------------------------------------------------------------

struct MockStatePort {
    inner: RefCell<SupervisorState>,
}

impl MockStatePort {
    fn new() -> Self {
        Self {
            inner: RefCell::new(SupervisorState::default()),
        }
    }
}

impl StatePort for MockStatePort {
    fn load(&self) -> Result<SupervisorState, StateError> {
        Ok(self.inner.borrow().clone())
    }
    fn save(&mut self, state: &SupervisorState) -> Result<(), StateError> {
        *self.inner.borrow_mut() = state.clone();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Smoke tests — prove the four ports are implementable and compose.
// ---------------------------------------------------------------------------

fn sample_spec(name: &str) -> DaemonSpec {
    DaemonSpec {
        name: name.into(),
        binary: "/bin/true".into(),
        args: vec![],
        throttle_seconds: 5,
        env: BTreeMap::new(),
        log_stdout: None,
        log_stderr: None,
        kill_switch: None,
        enabled: true,
    }
}

#[test]
fn mock_process_port_drives_state_machine_through_full_cycle() {
    let mut proc = MockProcessPort::new();
    let spec = sample_spec("tg-bot");

    let spawned = proc.spawn(&spec).expect("spawn");
    assert_eq!(spawned.pid, 1000);

    // Start the typestate at Spawning and step it through.
    let c = Child::<Spawning>::new(&spec.name);
    let c = c.spawned(spawned.pid, spawned.started_at);
    assert_eq!(c.pid(), 1000);

    // Schedule an exit code, then reap.
    proc.schedule_exit(1000, Some(0));
    let outcome = proc.reap(1000).expect("reap");
    let code = match outcome {
        ReapOutcome::Exited(n) => Some(n),
        ReapOutcome::Signaled => None,
        ReapOutcome::Alive => panic!("expected exit after scheduled exit"),
    };

    let c = c.exited(code, Utc::now());
    assert_eq!(c.exit_code(), Some(0));

    // Throttle, elapse, respawn.
    let until = Utc::now() + chrono::Duration::try_seconds(5).unwrap();
    let c = c.throttle(until);
    let c = c.elapsed(until + chrono::Duration::try_seconds(1).unwrap());
    let c = c.spawn();
    assert_eq!(c.respawn_count(), 1);
}

#[test]
fn mock_signal_call_records_ordering() {
    let mut proc = MockProcessPort::new();
    proc.signal(1234, Signal::Term).unwrap();
    proc.signal(1234, Signal::Kill).unwrap();
    assert_eq!(
        proc.signaled,
        vec![(1234, Signal::Term), (1234, Signal::Kill)]
    );
}

#[test]
fn mock_config_watch_drains_in_order_then_returns_none() {
    let t1 = Utc.timestamp_opt(100, 0).unwrap();
    let t2 = Utc.timestamp_opt(200, 0).unwrap();
    let mut watch = MockConfigWatchPort::new(vec![t1, t2]);
    assert_eq!(watch.next().unwrap(), Some(ConfigChange { at: t1 }));
    assert_eq!(watch.next().unwrap(), Some(ConfigChange { at: t2 }));
    assert_eq!(watch.next().unwrap(), None);
}

#[test]
fn mock_clock_advances_monotonically() {
    let c = MockClock::new(Utc.timestamp_opt(0, 0).unwrap());
    assert_eq!(c.now(), Utc.timestamp_opt(0, 0).unwrap());
    c.advance_secs(30);
    assert_eq!(c.now(), Utc.timestamp_opt(30, 0).unwrap());
}

#[test]
fn mock_state_port_roundtrips_through_save_load() {
    let mut s = SupervisorState::default();
    s.children.insert(
        "x".into(),
        PersistedChild {
            name: "x".into(),
            status: ChildStatus::Running,
            pid: Some(1000),
            last_exit_code: None,
            last_spawn_at: Some(Utc.timestamp_opt(1, 0).unwrap()),
            last_exit_at: None,
            respawn_count: 0,
        },
    );
    let mut port = MockStatePort::new();
    port.save(&s).unwrap();
    let back = port.load().unwrap();
    assert_eq!(s, back);
}

// Simple smoke: the four port traits are object-safe (`Box<dyn _>` compiles).
#[test]
fn port_traits_are_object_safe() {
    let _: Box<dyn ProcessPort> = Box::new(MockProcessPort::new());
    let _: Box<dyn ConfigWatchPort> = Box::new(MockConfigWatchPort::new(vec![]));
    let _: Box<dyn ClockPort> = Box::new(MockClock::new(Utc::now()));
    let _: Box<dyn StatePort> = Box::new(MockStatePort::new());
}
