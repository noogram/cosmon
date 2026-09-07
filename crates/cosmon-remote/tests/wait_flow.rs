// SPDX-License-Identifier: AGPL-3.0-only

//! `wait` — the client-side block, against a mocked adapter.
//!
//! The brief's falsifiers 3, 4, 5 and 6 (client half). The server halves of 1,
//! 2 and 3 are in `cosmon-rpp-adapter/tests/v1_status.rs`.
//!
//! 3. `--timeout 2` on a molecule that never moves returns the timeout outcome
//!    after ~2 s, and creates no server-side state keyed by the waiter — pinned
//!    here by the routes the server saw: only the status read, N times, with
//!    nothing that could hold a waiter.
//! 4. `--for completed` on a molecule that collapses returns `OtherTerminal`,
//!    which the binary maps to its own exit code — neither success nor timeout.
//! 5. `--poll-interval 100 --timeout 3` still terminates at ~3 s: the interval
//!    is clamped to the remaining budget.
//! 6. `do` and `wait` share the poller. Asserted from both ends: `do`'s test
//!    file mounts the full molecule read with `expect(0)` (it is never dialled
//!    any more), and this file pins that `poll_until` is the only loop by
//!    driving `do`'s follow phase and `wait` through the same function.
//!
//! Timing assertions have a floor and a generous ceiling. The floor is the
//! real claim — a `--timeout 2` that returns in 5 ms did not wait — and the
//! ceiling is loose enough not to flake on a loaded CI box.

use std::time::{Duration, Instant};

use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_remote::wait::{poll_until, WaitOptions, WaitOutcome};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile_for(server: &MockServer) -> Profile {
    Profile {
        host: server.uri(),
        sub: "tenant-demo-operator".into(),
        aud: "cosmon-rpp-tenant".into(),
        oidc_url: server.uri(),
        issuer: None,
        client_id: None,
        noyau: Some("default".into()),
        scopes: vec!["cosmon:molecule:read".into()],
        artifacts_dir: None,
        timeout_secs: 5,
        phone_home: true,
    }
}

fn client(server: &MockServer) -> Client {
    Client::new(&profile_for(server), Some("fake-jwt".into())).expect("client builds")
}

/// A status answer, with the `terminal` flag the server owns.
fn status_body(id: &str, status: &str, phase: &str, terminal: bool, at: &str) -> serde_json::Value {
    json!({
        "request_id": format!("req-{status}"),
        "molecule_id": id,
        "status": status,
        "phase": phase,
        "updated_at": at,
        "terminal": terminal,
    })
}

fn opts(targets: &[&str], timeout: Duration, interval: Duration) -> WaitOptions {
    WaitOptions {
        targets: targets.iter().map(|s| (*s).to_owned()).collect(),
        timeout,
        poll_interval: interval,
    }
}

/// Falsifier 3 — a molecule that never moves times out on the clock, and
/// nothing on the server was ever asked to hold the waiter.
#[tokio::test]
async fn a_molecule_that_never_moves_times_out_on_the_clock() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0001/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0001",
            "running",
            "live",
            false,
            "2026-09-07T10:00:00Z",
        )))
        .mount(&server)
        .await;

    let started = Instant::now();
    let outcome = poll_until(
        &client(&server),
        "task-wait-0001",
        &opts(
            &["completed"],
            Duration::from_secs(2),
            Duration::from_millis(200),
        ),
        |_, _| {},
    )
    .await
    .expect("a timeout is an outcome, never a transport error");
    let elapsed = started.elapsed();

    let WaitOutcome::TimedOut(report) = &outcome else {
        panic!("expected a timeout, got {outcome:?}");
    };
    assert_eq!(report.status, "running");
    assert_eq!(outcome.slug(), "timeout");
    assert!(
        elapsed >= Duration::from_secs(2),
        "returned in {elapsed:?} — that is not a two-second wait",
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "overshot the deadline badly: {elapsed:?}",
    );
    assert!(
        report.polls >= 2,
        "a 2 s wait at 200 ms polled {} times",
        report.polls
    );

    // Nothing on the server holds this waiter: every request it saw is the
    // status read. There is no route that could keep one, and no request
    // that could register one.
    let seen = server.received_requests().await.expect("recording enabled");
    assert!(!seen.is_empty());
    for req in &seen {
        assert_eq!(
            req.method,
            wiremock::http::Method::GET,
            "a wait writes nothing"
        );
        assert_eq!(
            req.url.path(),
            "/v1/molecules/task-wait-0001/status",
            "the ONLY route a wait dials",
        );
    }
    assert_eq!(
        usize::try_from(report.polls).unwrap(),
        seen.len(),
        "every poll is one request and no request is a poll's shadow",
    );
}

/// Falsifier 4 — a molecule that collapses while you waited for `completed`
/// is neither a success nor a timeout.
#[tokio::test]
async fn a_collapse_under_for_completed_is_its_own_outcome() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0002/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0002",
            "running",
            "live",
            false,
            "2026-09-07T10:00:00Z",
        )))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0002/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0002",
            "collapsed",
            "failed",
            true,
            "2026-09-07T10:00:30Z",
        )))
        .mount(&server)
        .await;

    let mut transitions = Vec::new();
    let outcome = poll_until(
        &client(&server),
        "task-wait-0002",
        &opts(
            &["completed"],
            Duration::from_secs(30),
            Duration::from_millis(20),
        ),
        |from, to| transitions.push(format!("{from} → {to}")),
    )
    .await
    .unwrap();

    let WaitOutcome::OtherTerminal(report) = &outcome else {
        panic!("expected OtherTerminal, got {outcome:?}");
    };
    assert_eq!(report.status, "collapsed");
    assert!(report.terminal);
    assert_eq!(outcome.slug(), "other_terminal");
    assert_eq!(transitions, ["running → collapsed"]);
    assert!(
        report.elapsed < Duration::from_secs(25),
        "it must stop AT the collapse, not run the clock out: {:?}",
        report.elapsed,
    );
}

/// The success case, and the idempotence `cs wait` has: an already-terminal
/// molecule returns on the first poll, with no sleep at all.
#[tokio::test]
async fn an_already_terminal_molecule_returns_on_the_first_poll() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0003/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0003",
            "completed",
            "done",
            true,
            "2026-09-07T10:00:00Z",
        )))
        .expect(1)
        .mount(&server)
        .await;

    let started = Instant::now();
    let outcome = poll_until(
        &client(&server),
        "task-wait-0003",
        &opts(
            &["completed", "collapsed"],
            Duration::from_secs(600),
            Duration::from_secs(5),
        ),
        |_, _| {},
    )
    .await
    .unwrap();

    let WaitOutcome::Reached(report) = &outcome else {
        panic!("expected Reached, got {outcome:?}");
    };
    assert_eq!(report.status, "completed");
    assert_eq!(report.polls, 1, "one poll, no sleep");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "waited {:?} on a molecule that was already over",
        started.elapsed(),
    );
}

/// Falsifier 5 — a poll interval far larger than the timeout must not outlive
/// the budget: it is clamped to what is left.
#[tokio::test]
async fn a_poll_interval_larger_than_the_timeout_still_terminates_on_time() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0004/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0004",
            "running",
            "live",
            false,
            "2026-09-07T10:00:00Z",
        )))
        .mount(&server)
        .await;

    let started = Instant::now();
    let outcome = poll_until(
        &client(&server),
        "task-wait-0004",
        &opts(
            &["completed"],
            Duration::from_secs(3),
            Duration::from_secs(100),
        ),
        |_, _| {},
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(outcome.slug(), "timeout");
    assert!(
        elapsed >= Duration::from_secs(3),
        "returned in {elapsed:?} — the deadline was 3 s",
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the 100 s interval was not clamped to the remaining budget: {elapsed:?}",
    );
}

/// Falsifier 2, client half — a `304` is an answer, not a hole. A poller that
/// treats it as "no data" would never terminate on an unmoved terminal
/// molecule; the loop must carry the previous answer forward.
#[tokio::test]
async fn a_304_carries_the_previous_answer_forward() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0005/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "W/\"running:2026-09-07T10:00:00+00:00\"")
                .set_body_json(status_body(
                    "task-wait-0005",
                    "running",
                    "live",
                    false,
                    "2026-09-07T10:00:00Z",
                )),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0005/status"))
        .respond_with(
            ResponseTemplate::new(304)
                .insert_header("etag", "W/\"running:2026-09-07T10:00:00+00:00\""),
        )
        .mount(&server)
        .await;

    let outcome = poll_until(
        &client(&server),
        "task-wait-0005",
        &opts(
            &["running"],
            Duration::from_secs(5),
            Duration::from_millis(20),
        ),
        |_, _| {},
    )
    .await
    .unwrap();

    let WaitOutcome::Reached(report) = &outcome else {
        panic!("expected Reached, got {outcome:?}");
    };
    assert_eq!(report.status, "running");
    assert_eq!(
        report.polls, 1,
        "the first answer already satisfied the target"
    );
}

/// The other half: the poller actually sends `If-None-Match`, and the `304`
/// it gets back does not erase what the molecule was.
#[tokio::test]
async fn the_poller_sends_the_tag_and_a_304_does_not_erase_the_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0006/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "W/\"running:2026-09-07T10:00:00+00:00\"")
                .set_body_json(status_body(
                    "task-wait-0006",
                    "running",
                    "live",
                    false,
                    "2026-09-07T10:00:00Z",
                )),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0006/status"))
        .respond_with(
            ResponseTemplate::new(304)
                .insert_header("etag", "W/\"running:2026-09-07T10:00:00+00:00\""),
        )
        .mount(&server)
        .await;

    let outcome = poll_until(
        &client(&server),
        "task-wait-0006",
        &opts(
            &["completed"],
            Duration::from_millis(300),
            Duration::from_millis(20),
        ),
        |_, _| {},
    )
    .await
    .unwrap();
    let report = outcome.report();
    assert_eq!(outcome.slug(), "timeout");
    assert!(
        report.unchanged_polls > 0,
        "no poll was answered 304 — the client is not sending If-None-Match",
    );
    assert_eq!(
        report.status, "running",
        "a 304 must not erase what the molecule was",
    );
    let seen = server.received_requests().await.unwrap();
    assert!(
        seen.iter().any(|r| r.headers.contains_key("if-none-match")),
        "the poller must echo the tag it was given",
    );
}

// ---------------------------------------------------------------------------
// The exit codes, from the real binary.
// ---------------------------------------------------------------------------

/// Where `ProfileStore::default_location` resolves `<config_dir>/cosmon-remote`
/// for a given `$HOME`. Reproduced (rather than pulled from `dirs`) so the test
/// plants the profile at exactly the path a `$HOME`-scoped child looks under —
/// same shape as `login_bind.rs`.
fn config_root_under(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/cosmon-remote")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".config/cosmon-remote")
    }
}

fn write_profile(home: &std::path::Path, server: &MockServer) {
    let root = config_root_under(home);
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("config.toml"), "default_profile = \"test\"\n").unwrap();
    std::fs::write(
        root.join("profiles").join("test.toml"),
        format!(
            "host = {:?}\nsub = \"operator\"\naud = \"cs-rpp-adapter\"\noidc_url = {:?}\n",
            server.uri(),
            server.uri()
        ),
    )
    .unwrap();
}

/// Run the real binary's `wait` under a scratch `$HOME` and return its exit
/// code. The bearer comes from the environment so no login flow runs.
fn run_wait(home: &std::path::Path, args: &[&str]) -> i32 {
    std::process::Command::new(env!("CARGO_BIN_EXE_cosmon-remote"))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("COSMON_REMOTE_TOKEN", "fake-jwt")
        .arg("wait")
        .args(args)
        .output()
        .expect("cosmon-remote spawns")
        .status
        .code()
        .expect("the process exits rather than being signalled")
}

/// Falsifiers 3 and 4, at the surface a script actually sees: three outcomes,
/// three exit codes, and the two failures are told apart.
///
/// A script that reads one code for both would retry a collapsed molecule
/// forever, which is the case this separation exists for.
#[tokio::test(flavor = "multi_thread")]
async fn the_three_outcomes_have_three_exit_codes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0100/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0100",
            "running",
            "live",
            false,
            "2026-09-07T10:00:00Z",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0101/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0101",
            "collapsed",
            "failed",
            true,
            "2026-09-07T10:00:00Z",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-wait-0102/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(status_body(
            "task-wait-0102",
            "completed",
            "done",
            true,
            "2026-09-07T10:00:00Z",
        )))
        .mount(&server)
        .await;

    let home = tempfile::tempdir().unwrap();
    write_profile(home.path(), &server);

    let started = Instant::now();
    let timed_out = tokio::task::block_in_place(|| {
        run_wait(
            home.path(),
            &["task-wait-0100", "--timeout", "2", "--poll-interval", "1"],
        )
    });
    let elapsed = started.elapsed();
    assert_eq!(
        timed_out, 124,
        "timeout must mirror timeout(1) and `cs wait`"
    );
    assert!(
        elapsed >= Duration::from_secs(2),
        "exited in {elapsed:?} without waiting the 2 s asked for",
    );

    let other = tokio::task::block_in_place(|| {
        run_wait(
            home.path(),
            &["task-wait-0101", "--for", "completed", "--timeout", "30"],
        )
    });
    assert_eq!(
        other, 125,
        "a collapse under --for completed is neither success nor timeout",
    );

    let reached = tokio::task::block_in_place(|| {
        run_wait(home.path(), &["task-wait-0102", "--timeout", "30"])
    });
    assert_eq!(reached, 0);

    assert_ne!(timed_out, other, "the two failures must be told apart");
    assert_ne!(timed_out, 0);
    assert_ne!(other, 0);
}

/// Falsifier 6 — one MOLECULE-polling loop in the crate, asserted
/// structurally.
///
/// A second loop is a second cadence, a second clamp and a second choice of
/// route, and the last time this crate had two the newer one was polling the
/// expensive read. The marker is a file that both sleeps and reads a
/// molecule's status: that pair is a polling loop and nothing else is.
///
/// `oidc/flow.rs` sleeps too, and legitimately — it waits on a peer holding
/// the credential lock and issues no request of its own — so the pair, not the
/// sleep alone, is what this looks for.
#[test]
fn there_is_exactly_one_polling_loop_in_the_crate() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sleepers = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let body = std::fs::read_to_string(&path).expect("source is utf8");
                if body.contains("time::sleep") && body.contains("get_status") {
                    sleepers.push(
                        path.strip_prefix(&src)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
    }
    sleepers.sort();
    assert_eq!(
        sleepers,
        ["wait.rs"],
        "a second sleep-between-requests loop appeared: `do` and `wait` must \
         share `wait::poll_until`, not grow a second cadence",
    );
}
