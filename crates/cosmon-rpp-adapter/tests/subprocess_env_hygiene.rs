// SPDX-License-Identifier: AGPL-3.0-only

//! §3.5 envelope — strip half.
//!
//! The subprocess envelope has two halves: **set** the per-tenant cwd
//! plus the three correlation vars, and **strip** the adapter-side
//! `COSMON_*` resolution vars that would otherwise leak into the
//! child and redirect its state lookups.
//!
//! Before this fix, an adapter started with `COSMON_STATE_DIR=/wrong`
//! would pollute every spawned `cs` subprocess, redirecting its
//! filestore lookups to the adapter's tree instead of the per-tenant
//! galaxy tree (T25 Gap 2 — remote-tackle V2 `POST tackle` failure).
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
use cosmon_rpp_adapter::subprocess::{SystemInvoker, STRIP_VARS};
use serde_json::Value;

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

#[tokio::test]
async fn asymmetry_adapter_cosmon_state_dir_does_not_reach_child() {
    let _lock = env_lock();
    // One guard per stripped var. We set all of them on the parent so a
    // single subprocess invocation proves the full STRIP_VARS list is
    // honoured, not just the three named in the T25 report.
    let _guards: Vec<EnvGuard> = STRIP_VARS
        .iter()
        .map(|k| EnvGuard::set(k, "/leaked/by/adapter"))
        .collect();

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");

    let result = invoker
        .invoke_owned(&spark, &["--json".into(), "__dump_env".into()])
        .await
        .expect("subprocess succeeds");

    let dumped: Value = serde_json::from_slice(&result.stdout)
        .expect("fake-cs --json __dump_env emits a JSON object");
    let map = dumped.as_object().expect("__dump_env returns an object");

    for key in STRIP_VARS {
        // COSMON_STATE_DIR is stripped THEN re-posed to the canonical
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
            "leaked stripped var into child: {key} = {:?}",
            map.get(*key)
        );
    }
}

#[tokio::test]
async fn pass_through_envelope_vars_reach_child() {
    let _lock = env_lock();
    // Strip every `COSMON_*` from the parent so we can prove the
    // envelope vars are SET by the invoker, not inherited.
    let _guards: Vec<EnvGuard> = STRIP_VARS.iter().map(|k| EnvGuard::remove(k)).collect();

    let tmp = tempfile::tempdir().expect("tempdir");
    let invoker = invoker_for(tmp.path());
    let spark = make_spark(tmp.path(), "a");

    let result = invoker
        .invoke_owned(&spark, &["--json".into(), "__dump_env".into()])
        .await
        .expect("subprocess succeeds");

    let dumped: Value = serde_json::from_slice(&result.stdout)
        .expect("fake-cs --json __dump_env emits a JSON object");
    let map = dumped.as_object().expect("__dump_env returns an object");

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
