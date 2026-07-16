// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-server-image-init-discipline` — boot-time materialization.
//!
//! These tests prove behavioural **equivalence** with the former
//! `cosmon-server-init.sh` ENTRYPOINT (the 8 steps), **idempotence**
//! (safe to re-run on every container restart — B2 eager), and the new
//! **multi-noyau** capability (one galaxy tree per noyau). The
//! `cs init` shell-out is the real authority, so the tests stub `cs`
//! with a tiny script that materialises `config.toml` the way the real
//! `cs init --upgrade` does — exercising the orchestration (mkdir →
//! shell-out → git) without rebuilding the whole CLI. `git` is real.
//!
//! Step 3c (Anthropic key injection) is proven end-to-end through the
//! `fake-cs` `__dump_env` mode: the resolved key reaches the spawned
//! subprocess env, the binary equivalent of the script's `export`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cosmon_rpp_adapter::admission::Spark;
use cosmon_rpp_adapter::image_init::{ImageInit, StepOutcome};
use cosmon_rpp_adapter::nucleon_map::Noyau;
use cosmon_rpp_adapter::subprocess::SystemInvoker;
use serde_json::Value;

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Write a fake `cs` that echoes its inherited `ANTHROPIC_API_KEY` as a
/// JSON object (sentinel `__ABSENT__` when unset/empty). Used to prove
/// step 3c injection reaches the spawned subprocess env — `fake-cs`
/// only dumps `COSMON_*` vars, so it cannot observe this one.
fn write_env_echo_cs(dir: &Path) -> PathBuf {
    let path = dir.join("fake-cs-envecho.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '{\"ANTHROPIC_API_KEY\":\"%s\"}' \"${ANTHROPIC_API_KEY:-__ABSENT__}\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    make_executable(&path);
    path
}

/// Write a fake `cs` executable that implements just enough of
/// `cs init --upgrade` for the orchestration test: on that subcommand
/// it materialises `.cosmon/config.toml` (with a `[project]` section)
/// in the current working directory, the same artifact the real
/// `cs init --upgrade` backfills. Any other invocation is a no-op exit
/// 0. Returns the path to the script.
fn write_fake_cs(dir: &Path) -> PathBuf {
    let path = dir.join("fake-cs-init.sh");
    let script = r#"#!/bin/sh
# Minimal `cs` stub for image_init orchestration tests.
if [ "$1" = "init" ]; then
    mkdir -p .cosmon
    if [ ! -f .cosmon/config.toml ]; then
        printf '[project]\nproject_id = "test-pid"\n' > .cosmon/config.toml
    fi
    exit 0
fi
exit 0
"#;
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    make_executable(&path);
    path
}

fn image_init_for(td: &Path, cs_path: PathBuf) -> ImageInit {
    ImageInit {
        inbox_root: td.join("whispers/inbox"),
        galaxies_root: td.join("galaxies"),
        cs_path,
        claude_home: td.join("home"),
        // No formula seed dir by default — `cs init` seeds the builtins;
        // individual tests opt into the belt-and-braces copy.
        formulas_seed_dir: None,
    }
}

/// Assert every artifact the 8-step script produced exists for `noyau`.
fn assert_noyau_materialized(galaxies_root: &Path, noyau: &str) {
    let root = galaxies_root.join(noyau);
    // Step 2 — state subtree (the script's three dirs).
    for sub in ["events", "molecules", "fleets/default"] {
        assert!(
            root.join(".cosmon/state").join(sub).is_dir(),
            "noyau {noyau}: missing .cosmon/state/{sub}",
        );
    }
    // Step 2a — cs init produced config.toml.
    assert!(
        root.join(".cosmon/config.toml").is_file(),
        "noyau {noyau}: cs init did not produce config.toml",
    );
    // Step 2b — git repo with at least the initial commit.
    assert!(root.join(".git").is_dir(), "noyau {noyau}: missing .git");
}

#[test]
fn single_noyau_materializes_all_steps() {
    // Equivalence: one noyau, the exact tenant-demo V1 case. Every step the
    // shell ENTRYPOINT performed must land.
    let td = tempfile::tempdir().unwrap();
    let cs = write_fake_cs(td.path());
    let init = image_init_for(td.path(), cs);

    let report = init.run(&[Noyau::new("tenant-demo-sandbox")]);

    assert!(report.all_ok(), "report had a failed step: {report:?}");

    // Step 1 — whispers/inbox (instance-level).
    assert!(td.path().join("whispers/inbox").is_dir());

    // Steps 2/2a/2b — per noyau.
    assert_noyau_materialized(&td.path().join("galaxies"), "tenant-demo-sandbox");

    // Steps 3a/3b — Claude Code gates in the worker $HOME.
    let claude_json = td.path().join("home/.claude.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(v["hasCompletedOnboarding"], Value::Bool(true));

    let settings = td.path().join("home/.claude/settings.json");
    let s: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(s["skipDangerousModePermissionPrompt"], Value::Bool(true));
}

#[test]
fn rerun_is_idempotent() {
    // B2 eager runs on every restart — a second pass must be a no-op:
    // every step `AlreadyPresent`, config.toml untouched, exactly one
    // git commit, no churn.
    let td = tempfile::tempdir().unwrap();
    let cs = write_fake_cs(td.path());
    let init = image_init_for(td.path(), cs);
    let noyaux = [Noyau::new("tenant-demo-sandbox")];

    let first = init.run(&noyaux);
    assert!(first.all_ok());

    // Mutate config.toml so we can detect a clobbering re-init.
    let config = td
        .path()
        .join("galaxies/tenant-demo-sandbox/.cosmon/config.toml");
    let sentinel = "[project]\nproject_id = \"sentinel-do-not-clobber\"\n";
    std::fs::write(&config, sentinel).unwrap();

    let second = init.run(&noyaux);
    assert!(second.all_ok(), "second run failed: {second:?}");

    // Every per-noyau step idempotent (no re-creation, no re-init).
    let n = &second.noyaux[0];
    assert_eq!(n.state_dirs, StepOutcome::AlreadyPresent);
    assert_eq!(n.cs_init, StepOutcome::AlreadyPresent);
    assert_eq!(n.git_init, StepOutcome::AlreadyPresent);
    assert_eq!(second.inbox, StepOutcome::AlreadyPresent);
    assert_eq!(second.claude_onboarding, StepOutcome::AlreadyPresent);
    assert_eq!(second.claude_skip_dangerous, StepOutcome::AlreadyPresent);

    // config.toml was NOT clobbered — cs init was correctly skipped.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), sentinel);
}

#[test]
fn multi_noyau_materializes_each_independently() {
    // The whole point of Phase 1: more than one convive. Two noyaux,
    // two independent galaxy trees, each fully materialised.
    let td = tempfile::tempdir().unwrap();
    let cs = write_fake_cs(td.path());
    let init = image_init_for(td.path(), cs);

    let report = init.run(&[Noyau::new("tenant-demo-sandbox"), Noyau::new("democorp")]);
    assert!(report.all_ok(), "report had a failed step: {report:?}");
    assert_eq!(report.noyaux.len(), 2);

    let galaxies = td.path().join("galaxies");
    assert_noyau_materialized(&galaxies, "tenant-demo-sandbox");
    assert_noyau_materialized(&galaxies, "democorp");

    // The two trees are genuinely separate repos (separate .git).
    assert_ne!(
        galaxies.join("tenant-demo-sandbox/.git"),
        galaxies.join("democorp/.git"),
    );
    assert!(galaxies.join("tenant-demo-sandbox/.git").is_dir());
    assert!(galaxies.join("democorp/.git").is_dir());
}

#[test]
fn no_noyaux_still_materializes_instance_level() {
    // Empty HabilitationMap (a freshly-provisioned instance with no binding
    // yet) must not panic — instance-level steps still run, the
    // per-noyau loop is simply empty. Non-regression: the adapter boots.
    let td = tempfile::tempdir().unwrap();
    let cs = write_fake_cs(td.path());
    let init = image_init_for(td.path(), cs);

    let report = init.run(&[]);
    assert!(report.all_ok());
    assert!(report.noyaux.is_empty());
    assert!(td.path().join("whispers/inbox").is_dir());
    assert!(td.path().join("home/.claude.json").is_file());
}

fn spark_for(noyau: &str, request_id: &str) -> Spark {
    Spark {
        request_id: request_id.to_owned(),
        nucleon_id: "nuc-test".to_owned(),
        noyau: Noyau::new(noyau),
        verb: "tackle".to_owned(),
        molecule_id: None,
        inbox_path: PathBuf::from("/tmp/unused-inbox"),
    }
}

#[tokio::test]
async fn anthropic_key_reaches_spawn_env() {
    // Step 3c end-to-end: a key handed to the invoker lands in the
    // spawned subprocess env (where the worker `claude` reads it).
    let td = tempfile::tempdir().unwrap();
    let cs = write_env_echo_cs(td.path());
    let galaxies_root = td.path().join("galaxies");
    std::fs::create_dir_all(galaxies_root.join("tenant-demo")).unwrap();

    let invoker = SystemInvoker::new(cs, galaxies_root, Duration::from_secs(10))
        .with_anthropic_key(Some("sk-ant-test-12345".to_owned()));

    let result = invoker
        .invoke_owned(
            &spark_for("tenant-demo", "req-anthropic-1"),
            &["--json".into()],
        )
        .await
        .expect("subprocess succeeds");
    let dumped: Value = serde_json::from_slice(&result.stdout).unwrap();

    assert_eq!(
        dumped.get("ANTHROPIC_API_KEY").and_then(Value::as_str),
        Some("sk-ant-test-12345"),
        "anthropic key did not reach the spawned subprocess env",
    );
}

#[tokio::test]
async fn no_anthropic_key_does_not_inject() {
    // Symmetric: the default invoker (no key) must not invent an
    // ANTHROPIC_API_KEY in the child env. Remove any inherited value
    // first so the assertion is about injection, not inheritance.
    let prev = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::remove_var("ANTHROPIC_API_KEY");

    let td = tempfile::tempdir().unwrap();
    let cs = write_env_echo_cs(td.path());
    let galaxies_root = td.path().join("galaxies");
    std::fs::create_dir_all(galaxies_root.join("tenant-demo")).unwrap();

    let invoker = SystemInvoker::new(cs, galaxies_root, Duration::from_secs(10));
    let result = invoker
        .invoke_owned(
            &spark_for("tenant-demo", "req-anthropic-2"),
            &["--json".into()],
        )
        .await
        .expect("subprocess succeeds");

    // Restore the env before asserting (so a failure does not leak).
    if let Some(v) = prev {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    }

    let dumped: Value = serde_json::from_slice(&result.stdout).unwrap();
    // The stub prints the sentinel when the var is unset/empty — proving
    // nothing was injected.
    assert_eq!(
        dumped.get("ANTHROPIC_API_KEY").and_then(Value::as_str),
        Some("__ABSENT__"),
        "ANTHROPIC_API_KEY was injected (or inherited) without a configured key",
    );
}
