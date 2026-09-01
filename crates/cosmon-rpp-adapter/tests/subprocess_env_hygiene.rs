// SPDX-License-Identifier: AGPL-3.0-only

//! §3.5 envelope — the allow-list half.
//!
//! The subprocess envelope has two halves: **set** the per-tenant cwd
//! plus the three correlation vars, and **clear** the adapter's own
//! environment, re-admitting only the names on
//! [`cosmon_rpp_adapter::subprocess::PASSTHROUGH_VARS`].
//!
//! Before the first fix, an adapter started with
//! `COSMON_STATE_DIR=/wrong` would pollute every spawned `cs`
//! subprocess, redirecting its filestore lookups to the adapter's tree
//! instead of the per-tenant galaxy tree (T25 Gap 2 — remote-tackle V2
//! `POST tackle` failure). That fix was a deny-list, and a deny-list is
//! wrong by default for every variable added elsewhere afterwards:
//! `COSMON_SKIP_PRE_DONE_HOOK`, the human operator's kill-switch for
//! the blocking `pre_done` gate, was never on it and crossed the
//! perimeter into every `cs done` the drain launched
//! (delib-20260819-cda2, C2). The envelope is now an allow-list, and
//! these tests assert the *structural* property — nothing but the
//! named vars reaches the child — rather than the absence of a roster.
//!
//! These tests use the `fake-cs` `__dump_env` mode to inspect the
//! child's inherited environment without rebuilding the real `cs`.
//!
//! The `await_holding_lock` lint is allowed at the file level: the
//! sync mutex is the env-mutation serialisation hatch, and the
//! `#[tokio::test]` default runtime is `current_thread`, so the
//! await cannot move the task to another thread mid-lock.

#![allow(clippy::await_holding_lock)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use cosmon_oidc_testkit::fake_cs_path;
use cosmon_rpp_adapter::admission::Spark;
use cosmon_rpp_adapter::nucleon_map::Noyau;
use cosmon_rpp_adapter::subprocess::{is_passthrough, SystemInvoker};
use serde_json::Value;

/// Variables the envelope *sets* itself. Everything else in the child
/// must be allow-listed, or the perimeter leaks.
const ENVELOPE_SET_VARS: &[&str] = &[
    "COSMON_API_REQUEST",
    "COSMON_API_REQUEST_ID",
    "COSMON_API_NUCLEON",
    "COSMON_STATE_DIR",
    "COSMON_ARTIFACT_DIR",
];

/// Adapter-side variables that must never be inherited. A sample, not
/// a roster: under an allow-list the guarantee is structural (see
/// [`only_allow_listed_vars_reach_child`]), and this list exists to
/// name the historically load-bearing cases in a failing assertion.
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
    // A name nobody has ever written down anywhere in cosmon: the
    // allow-list must block it precisely because it is unknown.
    "COSMON_A_VARIABLE_INVENTED_TOMORROW",
];

/// Tests in this file mutate process-global env vars; serialise them
/// behind a shared mutex so cargo's default parallel scheduling does
/// not let one test's setup leak into another's assertion window.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard that restores (or removes) an env var on drop. Avoids
/// cross-test leakage when the asymmetry case sets `COSMON_*` vars.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn remove(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn make_spark(galaxies_root: &std::path::Path, noyau_name: &str) -> Spark {
    // Materialise the per-tenant cwd so the invoker pins it (the
    // best-effort `cwd.exists()` check inside `invoke_owned` would
    // otherwise inherit the test process cwd).
    let tenant_root = galaxies_root.join(noyau_name);
    std::fs::create_dir_all(&tenant_root).expect("create tenant root");

    Spark {
        request_id: "req-env-hygiene-1".to_owned(),
        nucleon_id: "nuc-test".to_owned(),
        noyau: Noyau::new(noyau_name),
        verb: "observe".to_owned(),
        molecule_id: None,
        inbox_path: PathBuf::from("/tmp/unused-inbox-path"),
    }
}

fn invoker_for(galaxies_root: &std::path::Path) -> SystemInvoker {
    SystemInvoker::new(
        fake_cs_path(),
        galaxies_root.to_path_buf(),
        Duration::from_secs(10),
    )
}

/// Spawn `fake-cs --json __dump_env` through the real envelope and
/// return the child's full environment as a map.
async fn dump_child_env(invoker: &SystemInvoker, spark: &Spark) -> serde_json::Map<String, Value> {
    let result = invoker
        .invoke_owned(spark, &["--json".into(), "__dump_env".into()])
        .await
        .expect("subprocess succeeds");
    let dumped: Value =
        serde_json::from_slice(&result.stdout).expect("fake-cs --json __dump_env emits JSON");
    dumped
        .as_object()
        .expect("__dump_env returns an object")
        .clone()
}

#[tokio::test]
async fn asymmetry_adapter_cosmon_state_dir_does_not_reach_child() {
    let _lock = env_lock();
    // One guard per poison var. We set all of them on the parent so a
    // single subprocess invocation proves the envelope holds for the
    // whole sample, not just the three named in the T25 report.
    let _guards: Vec<EnvGuard> = POISON_VARS
        .iter()
        .map(|k| EnvGuard::set(k, "/leaked/by/adapter"))
        .collect();

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");
    let map = dump_child_env(&invoker, &spark).await;

    for key in POISON_VARS {
        // COSMON_STATE_DIR is cleared THEN re-posed to the canonical
        // tenant store (B1 moussage resident, task-20260610-e5f6 — fix
        // of the `tackle 503` strip divergence). The hygiene invariant
        // it must satisfy is *stronger* than absence: the child sees
        // the per-tenant path, never the adapter's leaked value.
        if *key == "COSMON_STATE_DIR" {
            let expected = tmp
                .path()
                .join("a")
                .join(".cosmon")
                .join("state")
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                map.get(*key).and_then(Value::as_str),
                Some(expected.as_str()),
                "COSMON_STATE_DIR must be re-posed to the tenant store, \
                 not inherited from the adapter env"
            );
            continue;
        }
        assert!(
            !map.contains_key(*key),
            "leaked adapter var into child: {key} = {:?}",
            map.get(*key)
        );
    }
}

/// The named non-regression test for delib-20260819-cda2 C2.
///
/// `COSMON_SKIP_PRE_DONE_HOOK` disarms the blocking `pre_done` gate for
/// any `cs` that inherits it — including the `cs run` drain the adapter
/// spawns, and every `cs done` that drain launches at teardown
/// (`cmd/run.rs` step 9 spawns `cs done` with a plainly inherited
/// environment; nothing on that path sanitises it). One variable set
/// once in the container therefore disarmed the Definition-of-Done of
/// every subsequent harvest. It must die at the adapter perimeter.
#[tokio::test]
async fn operator_kill_switch_does_not_reach_child() {
    let _lock = env_lock();
    let _guard = EnvGuard::set("COSMON_SKIP_PRE_DONE_HOOK", "1");

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");
    let map = dump_child_env(&invoker, &spark).await;

    assert!(
        !map.contains_key("COSMON_SKIP_PRE_DONE_HOOK"),
        "the pre_done kill-switch crossed the adapter perimeter: {:?} — \
         a container has no operator to make that gesture",
        map.get("COSMON_SKIP_PRE_DONE_HOOK")
    );
}

/// The structural invariant the allow-list buys, stated once: every
/// variable in the child is either allow-listed or set by the envelope
/// itself. A future variable added anywhere in the workspace is closed
/// on arrival — this test fails the day the envelope goes back to
/// inheriting by default, whatever the new variable is called.
#[tokio::test]
async fn only_allow_listed_vars_reach_child() {
    let _lock = env_lock();
    let _guards: Vec<EnvGuard> = POISON_VARS
        .iter()
        .map(|k| EnvGuard::set(k, "/leaked/by/adapter"))
        .collect();

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");
    let map = dump_child_env(&invoker, &spark).await;

    let unexpected: Vec<&String> = map
        .keys()
        .filter(|k| !is_passthrough(k) && !ENVELOPE_SET_VARS.contains(&k.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "the child inherited variables that are neither allow-listed nor \
         set by the envelope: {unexpected:?}"
    );
}

/// The other failure mode of an allow-list: too narrow, and the child
/// cannot find `git`, `sh` or its own configuration. `PATH` in
/// particular must survive the clear — a `cs` that cannot resolve a
/// binary fails in a way that looks like anything but env hygiene.
#[tokio::test]
async fn process_basics_survive_the_clear() {
    let _lock = env_lock();
    let _path_guard = EnvGuard::set("PATH", "/usr/bin:/bin");
    let _home_guard = EnvGuard::set("HOME", "/home/user");

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");
    let map = dump_child_env(&invoker, &spark).await;

    assert_eq!(
        map.get("PATH").and_then(Value::as_str),
        Some("/usr/bin:/bin"),
        "PATH must cross the perimeter — the child shells out to git, sh and tmux"
    );
    assert_eq!(
        map.get("HOME").and_then(Value::as_str),
        Some("/home/user"),
        "HOME must cross the perimeter — git and the claude CLI read no config without it"
    );
}

#[tokio::test]
async fn pass_through_envelope_vars_reach_child() {
    let _lock = env_lock();
    // Remove every poison var from the parent so we can prove the
    // envelope vars are SET by the invoker, not inherited.
    let _guards: Vec<EnvGuard> = POISON_VARS.iter().map(|k| EnvGuard::remove(k)).collect();

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");
    let map = dump_child_env(&invoker, &spark).await;

    assert_eq!(
        map.get("COSMON_API_REQUEST").and_then(Value::as_str),
        Some("1"),
        "envelope COSMON_API_REQUEST missing in child env",
    );
    assert_eq!(
        map.get("COSMON_API_REQUEST_ID").and_then(Value::as_str),
        Some(spark.request_id.as_str()),
        "envelope COSMON_API_REQUEST_ID missing or wrong in child env",
    );
    assert_eq!(
        map.get("COSMON_API_NUCLEON").and_then(Value::as_str),
        Some(spark.nucleon_id.as_str()),
        "envelope COSMON_API_NUCLEON missing or wrong in child env",
    );
}
