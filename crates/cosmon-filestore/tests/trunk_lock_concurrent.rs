// SPDX-License-Identifier: AGPL-3.0-only

//! Cross-process integration test for the trunk write-discipline lock
//! (ADR-110 Phase 1).
//!
//! The unit tests in `src/lib.rs` cover the in-process behaviour (per-FD
//! flock semantics, holder hint, RAII release). This file exercises the
//! *real* cross-process scenario the molecule names — two `cs done`
//! processes hitting the same cosmon state dir concurrently — plus an
//! in-process **lock-order** regression test that witnesses the
//! deadlock-free order proven by `smithy/docs/formal/MCStitch.tla`.
//!
//! It does so by spawning two copies of the `trunk_lock_holder` example
//! binary (built alongside the crate), one with a long hold and one with a
//! short hold, and asserting:
//!
//! 1. The second process **blocks** until the first releases (matches
//!    *« 2 workers concurrents → un wait, un passe »*).
//! 2. With `COSMON_TRUNK_LOCK_NONBLOCKING=1`, the second process **fast-fails**
//!    with a `LockFailed` error carrying the first holder's PID + cmd hint
//!    (matches *« Refuser proprement (clear error + retry hint) »*).
//! 3. The lock file at `<state_dir>/trunk.lock` is left empty on clean
//!    release — no phantom-writer claim survives the holder dropping.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Env var that names a prebuilt `trunk_lock_holder` helper explicitly.
const HELPER_BIN_ENV: &str = "COSMON_TEST_TRUNK_LOCK_HOLDER_BIN";

/// Locate the *prebuilt* `trunk_lock_holder` example binary.
///
/// **Resolution is explicit and never builds.** The previous version of this
/// function shelled out to `cargo build --example trunk_lock_holder` when the
/// helper was missing, guarded by a `Once` so the three tests in this file
/// would not queue three cargo runs behind cargo's build lock. That `Once`
/// only covers this process: under a parallel runner several test *targets*
/// can miss their artifact simultaneously, and each nested cargo then blocks
/// on a lock the outer `cargo test` may still hold. The observable symptom is
/// not a slow test but a test that never returns — and when it does return,
/// the artifact was written while a sibling was already executing it. A build
/// whose timing is decided by the runner's scheduling does not belong inside
/// a test.
///
/// So the helper must exist before the test runs. It does under any
/// package-wide invocation — `cargo test -p cosmon-filestore`, and therefore
/// the `cargo test --workspace` verification gate, builds every example
/// target of the package under test. It does *not* under a target-scoped
/// `cargo test --test trunk_lock_concurrent`, which builds only the named
/// test; that case now reads as a named missing prerequisite instead of
/// silently growing a build step.
///
/// **The path is derived from our own executable**, not from
/// `CARGO_MANIFEST_DIR`: the test binary lives at
/// `<target>/<profile>/deps/trunk_lock_concurrent-<hash>`, so the sibling
/// `examples/` directory is two levels up. That is the one anchor that stays
/// correct under an isolated `CARGO_TARGET_DIR`; the old manifest-relative
/// `<workspace>/target/<profile>/...` guess resolved to a directory cargo had
/// never written to, and both cross-process tests failed with `NotFound` for
/// anyone building outside the default target dir.
fn helper_bin() -> PathBuf {
    match try_helper_bin() {
        Ok(path) => path,
        Err(msg) => panic!("{msg}"),
    }
}

/// [`helper_bin`] as a `Result`, so the resolution rule itself is testable.
fn try_helper_bin() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir: &Path = exe
        .parent() // <target>/<profile>/deps
        .and_then(Path::parent) // <target>/<profile>
        .expect("test binary lives under <target>/<profile>/deps");
    let candidate = profile_dir.join("examples").join("trunk_lock_holder");
    resolve_prebuilt(
        HELPER_BIN_ENV,
        std::env::var_os(HELPER_BIN_ENV).map(PathBuf::from),
        &candidate,
        "trunk_lock_holder",
    )
}

/// Env override first, then the profile-directory candidate, then a clear
/// error. An override pointing at a nonexistent path is an error rather than
/// a silent fallback: a typo in a CI variable must not degrade into
/// "test something else".
///
/// The override arrives as a parameter rather than being read here, so the
/// rule is exercisable without mutating the process environment — which in a
/// multi-threaded test binary that also spawns subprocesses is not a safe
/// thing to do.
fn resolve_prebuilt(
    env_var: &str,
    override_path: Option<PathBuf>,
    candidate: &Path,
    artifact: &str,
) -> Result<PathBuf, String> {
    if let Some(from_env) = override_path {
        if from_env.is_file() {
            return Ok(from_env);
        }
        return Err(format!(
            "{env_var} is set to `{}`, but no file exists there. \
             Point it at a prebuilt `{artifact}` binary, or unset it to use \
             the profile directory.",
            from_env.display(),
        ));
    }
    if candidate.is_file() {
        return Ok(candidate.to_path_buf());
    }
    Err(format!(
        "prebuilt `{artifact}` example not found at `{}`. \
         This test never builds it implicitly — a nested `cargo build` races \
         the parallel test runner. Build it first with \
         `cargo build -p cosmon-filestore --example {artifact}`, run the whole \
         gate (`cargo test --workspace`), or set {env_var} to an existing \
         binary.",
        candidate.display(),
    ))
}

/// The resolver reports a missing prerequisite; it does not build one.
///
/// This is the property the nested `cargo build` used to violate. A
/// candidate path that cannot exist forces the error branch, and the message
/// must name both remedies — the build command and the override env var — so
/// an operator reading a CI log knows what to do without reading this file.
#[test]
fn missing_helper_is_a_named_prerequisite_not_a_build() {
    let bogus = Path::new("/nonexistent/cosmon/trunk_lock_holder");
    let err = resolve_prebuilt(HELPER_BIN_ENV, None, bogus, "trunk_lock_holder")
        .expect_err("a nonexistent candidate must not resolve");
    assert!(
        err.contains("cargo build -p cosmon-filestore --example trunk_lock_holder"),
        "error must name the build command; got: {err}"
    );
    assert!(
        err.contains(HELPER_BIN_ENV),
        "error must name the override env var; got: {err}"
    );
}

/// An override pointing at nothing is an error, not a quiet fallback.
///
/// A typo in a CI variable must fail loudly rather than resolve to some
/// other artifact under the profile directory — otherwise the run reports on
/// a binary nobody asked it to test.
#[test]
fn override_pointing_at_nothing_does_not_fall_back() {
    let err = resolve_prebuilt(
        HELPER_BIN_ENV,
        Some(PathBuf::from("/nonexistent/cosmon/override")),
        Path::new("/nonexistent/cosmon/candidate"),
        "trunk_lock_holder",
    )
    .expect_err("a dangling override must not resolve");
    assert!(
        err.contains("/nonexistent/cosmon/override"),
        "error must quote the override path it rejected; got: {err}"
    );
}

/// Spawn the helper holding the lock and return its (child, `acquired_at`)
/// once the child has emitted `ACQUIRED <pid>`. The child stays alive
/// holding the lock until the caller waits on it.
fn spawn_holder_and_wait_for_acquire(
    state_dir: &Path,
    hold_ms: u64,
    cmd_hint: &str,
) -> (std::process::Child, String) {
    let mut child = Command::new(helper_bin())
        .arg(state_dir)
        .arg(hold_ms.to_string())
        .arg(cmd_hint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holder");

    let stdout = child.stdout.take().expect("holder stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read ACQUIRED");
    assert!(
        line.starts_with("ACQUIRED "),
        "expected ACQUIRED, got {line:?}"
    );
    // Reattach stdout so the parent doesn't accidentally block the child on
    // its later `RELEASED` write — the BufReader took ownership; we discard
    // the rest of stdout via a draining thread.
    std::thread::spawn(move || {
        let mut sink = String::new();
        let _ = reader.read_to_string(&mut sink);
    });

    let pid = line.trim().strip_prefix("ACQUIRED ").unwrap().to_owned();
    (child, pid)
}

#[test]
fn two_concurrent_holders_serialise_via_trunk_lock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    // Holder A grabs the lock and holds it for 400ms.
    let (mut a, _pid_a) = spawn_holder_and_wait_for_acquire(state_dir, 400, "cs done A");

    // Holder B tries to grab — its hold is 10ms so we don't add latency on
    // top of A's hold once A releases.
    let start = Instant::now();
    let mut b = Command::new(helper_bin())
        .arg(state_dir)
        .arg("10")
        .arg("cs done B")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn holder B");
    let status_b = b.wait().expect("wait B");
    let elapsed = start.elapsed();

    let status_a = a.wait().expect("wait A");
    assert!(status_a.success(), "holder A exited non-zero");
    assert!(status_b.success(), "holder B exited non-zero");

    // B must have waited at least most of A's hold — allow 100ms slack for
    // scheduling, process startup, file-system caching jitter.
    assert!(
        elapsed >= Duration::from_millis(300),
        "B did not block on A's lock: elapsed {elapsed:?} (expected >= 300ms while A held 400ms)"
    );

    // No phantom-writer claim after both released.
    let body = std::fs::read_to_string(state_dir.join("trunk.lock")).unwrap_or_default();
    assert!(
        body.trim().is_empty(),
        "trunk.lock holder hint not cleared after release: {body:?}"
    );
}

#[test]
fn nonblocking_env_fast_fails_with_holder_hint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path();

    // Holder A grabs the lock and holds it for 1s (more than the contender
    // could ever wait — proves the contender did not block).
    let (mut a, pid_a) = spawn_holder_and_wait_for_acquire(state_dir, 1000, "cs done A");

    // Contender runs with COSMON_TRUNK_LOCK_NONBLOCKING=1 — expects an
    // immediate failure surface, *not* a blocked wait.
    let start = Instant::now();
    let out = Command::new(helper_bin())
        .arg(state_dir)
        .arg("10")
        .arg("cs done contender")
        .env("COSMON_TRUNK_LOCK_NONBLOCKING", "1")
        .output()
        .expect("spawn contender");
    let elapsed = start.elapsed();

    assert!(
        !out.status.success(),
        "contender should have exited non-zero under NONBLOCKING contention; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "contender should have fast-failed; elapsed {elapsed:?}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("trunk lock held by"),
        "expected holder hint in error, got stderr: {stderr}"
    );
    assert!(
        stderr.contains(&pid_a) || stderr.contains("cs done A"),
        "expected pid {pid_a} or cmd in error, got stderr: {stderr}"
    );

    // Let A wind down.
    let _ = a.wait();
}

/// Lock-order regression test — witnesses the deadlock-free total order
/// proven by the TLA+ model `smithy/docs/formal/MCStitch.tla`
/// (`MCStitch.cfg` = SAFE; `MCStitchDeadlock.cfg` = the inversion we avoid).
///
/// The global order is **trunk ⊃ fleet**: any path that needs both takes
/// `trunk` first. Two families contend here, both faithful to the impl:
///
/// - **`cs done`** acquires the trunk lock (outer), takes the fleet lock
///   *inside* it for the merge-window state write (trunk ⊃ fleet), then
///   **drops the trunk lock before** its terminal fleet-purge — so it never
///   holds `fleet ⊃ trunk`. (`done.rs`, abda recovery.)
/// - **`cs stitch`** holds the trunk lock **alone** — it rewrites no fleet
///   state — so it can never be the fleet-side of an inversion.
///   (`stitch.rs`, TLA+ fix option 1.)
///
/// Both orders agree, so Coffman *circular-wait* is impossible. The test
/// drives many concurrent rounds and asserts every worker finishes inside a
/// generous watchdog window: a lock-order inversion would hang here and trip
/// the `recv_timeout`.
#[test]
fn lock_order_done_and_stitch_terminate_without_deadlock() {
    use std::sync::mpsc;
    use std::thread;

    use cosmon_filestore::FileStore;

    const ROUNDS: usize = 30;
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir: PathBuf = tmp.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<&'static str>();

    // "cs done" worker: trunk (outer) ⊃ fleet (inner), then drop trunk
    // before a second fleet acquisition that models the terminal purge.
    let done_dir = state_dir.clone();
    let done_tx = tx.clone();
    let done = thread::spawn(move || {
        let store = FileStore::new(&done_dir);
        for _ in 0..ROUNDS {
            let guard = store.acquire_trunk_lock("cs done worker").unwrap();
            // Merge-window state write: fleet lock nested INSIDE trunk lock.
            store
                .with_fleet_lock(|_s| -> Result<(), cosmon_core::error::CosmonError> { Ok(()) })
                .unwrap();
            // Trunk write done — release trunk BEFORE the fleet-purge so we
            // never hold both at the purge boundary (abda lock-order rule).
            drop(guard);
            store
                .with_fleet_lock(|_s| -> Result<(), cosmon_core::error::CosmonError> { Ok(()) })
                .unwrap();
        }
        done_tx.send("done").unwrap();
    });

    // "cs stitch" worker: trunk lock ALONE for the whole batch.
    let stitch_dir = state_dir.clone();
    let stitch_tx = tx.clone();
    let stitch = thread::spawn(move || {
        let store = FileStore::new(&stitch_dir);
        for _ in 0..ROUNDS {
            store
                .with_trunk_lock(
                    "cs stitch worker",
                    |_s| -> Result<(), cosmon_core::error::CosmonError> { Ok(()) },
                )
                .unwrap();
        }
        stitch_tx.send("stitch").unwrap();
    });

    drop(tx);

    // Watchdog: a circular-wait would leave one worker blocked forever.
    let deadline = Duration::from_secs(20);
    let mut finished = 0;
    for _ in 0..2 {
        match rx.recv_timeout(deadline) {
            Ok(_who) => finished += 1,
            Err(e) => panic!(
                "deadlock suspected ({e}): only {finished}/2 lock-order workers finished within {deadline:?} \
                 — a trunk/fleet lock-order inversion (Coffman circular-wait) would hang here"
            ),
        }
    }
    assert_eq!(finished, 2, "both lock-order workers must finish");

    done.join().unwrap();
    stitch.join().unwrap();
}

// Pull `read_to_string` into scope for the draining thread inside
// `spawn_holder_and_wait_for_acquire`.
use std::io::Read as _;
