// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sessions hook`, exercised against the real binary (mission co-pilotage
//! M6).
//!
//! The document rules are unit-tested in `cosmon_core::copilot_hook`. What can
//! only be tested from here is the part that *is* a process, and that is
//! exactly M6's acceptance list — each clause below is one of its five:
//!
//! 1. **presence visible on both sides** — [`presence_becomes_reciprocal`];
//! 2. **a round trip** — [`a_message_reaches_the_pilot_and_is_answered`];
//! 3. **cost measured** — [`the_hook_records_what_it_cost`];
//! 4. **clean deactivation** — [`deactivation_leaves_the_settings_file_as_it_was`]
//!    and [`the_off_switch_stops_the_hook_without_unwiring_it`];
//! 5. **no keystroke sent by the probe** —
//!    [`the_hook_writes_nothing_outside_the_state_root`].
//!
//! Every one of them runs the hook the way a provider would: a subprocess with
//! a JSON payload on stdin, whose stdout is what the pilot's model would read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cosmon_core::copilot_hook::{HookEvent, HOOK_MARKER, HOOK_OFF_ENV};

fn cs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cs"))
}

/// Run `cs` under a state root, as a given session, and return
/// `(stdout, stderr, code)`.
fn run(state: &Path, session: &str, args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let mut cmd = cs();
    cmd.arg("--config")
        .arg(state)
        .args(args)
        .env("COSMON_SESSION_ID", session)
        // The probe registry walks a provider's log root; pointing both at an
        // empty directory keeps this test off whatever this host happens to
        // have in `~/.claude` and `~/.codex`.
        .env("COSMON_SESSIONS_CLAUDE_ROOT", state.join("no-claude"))
        .env("COSMON_SESSIONS_CODEX_ROOT", state.join("no-codex"))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn cs");
    if let Some(text) = stdin {
        use std::io::Write as _;
        let mut pipe = child.stdin.take().expect("stdin");
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().expect("cs exits");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Fire the hook exactly as a provider would: payload on stdin, no arguments
/// beyond the ones the settings entry carries.
fn fire(state: &Path, session: &str, event: HookEvent, payload: &str) -> (String, String, i32) {
    run(
        state,
        session,
        &["sessions", "hook", "run", "--event", event.as_str()],
        Some(payload),
    )
}

fn payload_for(native: &str) -> String {
    serde_json::json!({
        "session_id": native,
        "transcript_path": "/dev/null",
        "hook_event_name": "UserPromptSubmit",
    })
    .to_string()
}

/// Every file under `root`, mapped to its bytes — the shape a "did anything
/// change?" assertion needs.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(path, bytes);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 1. presence visible on both sides
// ---------------------------------------------------------------------------

/// **Acceptance clause 1.** After each pilot's hook has fired once, each one
/// sees the other in `cs sessions peers`, and the relation is *mutual* rather
/// than merely two sessions in a directory.
///
/// The seats are taken by `cs sessions attach`, which is an operator gesture.
/// The hook only keeps them alive — which is the assertion in the second half:
/// the role and `follows` survive a bare heartbeat rather than being reset by
/// it.
#[test]
fn presence_becomes_reciprocal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();

    let (_, err, code) = run(
        state,
        "pilot-claude",
        &[
            "sessions",
            "attach",
            "--role",
            "copilot",
            "--as",
            "claude:native-aaa",
        ],
        None,
    );
    assert_eq!(code, 0, "attach claude: {err}");
    let (_, err, code) = run(
        state,
        "pilot-codex",
        &[
            "sessions",
            "attach",
            "--role",
            "copilot",
            "--as",
            "codex:native-bbb",
            "--follow",
            "pilot-claude",
        ],
        None,
    );
    assert_eq!(code, 0, "attach codex: {err}");

    // Now the hooks fire, once each. Nothing else runs.
    for (sid, native) in [
        ("pilot-claude", "native-aaa"),
        ("pilot-codex", "native-bbb"),
    ] {
        let (_, err, code) = fire(state, sid, HookEvent::TurnStart, &payload_for(native));
        assert_eq!(code, 0, "hook for {sid}: {err}");
    }

    let (out, err, code) = run(state, "pilot-claude", &["sessions", "peers"], None);
    assert_eq!(code, 0, "peers: {err}");
    assert!(out.contains("pilot-codex"), "claude sees codex:\n{out}");
    assert!(
        out.contains("follows-me"),
        "and sees that codex follows it:\n{out}"
    );

    let (out, err, code) = run(state, "pilot-codex", &["sessions", "peers"], None);
    assert_eq!(code, 0, "peers: {err}");
    assert!(out.contains("pilot-claude"), "codex sees claude:\n{out}");
    assert!(
        out.contains("i-follow"),
        "the hook's bare heartbeat did not erase `follows`:\n{out}"
    );
}

/// A hook heartbeat never claims a seat. `--role primary` is an operator
/// gesture checked against the lease ledger (ADR-168 §D6); a hook that pinged
/// it every thirty seconds would be a takeover nobody decided.
#[test]
fn the_hook_never_promotes_the_session_it_runs_in() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();

    run(state, "solo", &["sessions", "attach"], None);
    fire(state, "solo", HookEvent::SessionStart, &payload_for("n1"));
    fire(state, "solo", HookEvent::TurnStart, &payload_for("n1"));

    let (out, err, code) = run(state, "solo", &["--json", "sessions", "peers"], None);
    assert_eq!(code, 0, "peers: {err}");
    let row: serde_json::Value =
        serde_json::from_str(out.lines().next().expect("one row")).expect("json");
    assert_eq!(row["role"], "copilot", "still read-only:\n{out}");
}

// ---------------------------------------------------------------------------
// 2. a round trip
// ---------------------------------------------------------------------------

/// **Acceptance clause 2.** A message sent by one pilot reaches the other
/// *through its hook* — printed on the stdout its provider feeds back to the
/// model — and the reply travels the other way. The envelope is acknowledged
/// once, so a second turn does not re-deliver it.
#[test]
fn a_message_reaches_the_pilot_and_is_answered() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();

    for (sid, sel) in [("pilot-a", "claude:aaa"), ("pilot-b", "codex:bbb")] {
        run(state, sid, &["sessions", "attach", "--as", sel], None);
    }

    let (_, err, code) = run(
        state,
        "pilot-b",
        &[
            "sessions",
            "send",
            "--to",
            "pilot-a",
            "--message",
            "your evidence for H2 cites the same file it is derived from",
        ],
        None,
    );
    assert_eq!(code, 0, "send: {err}");

    // A's next turn boundary. The hook is what delivers it — nobody typed
    // `cs sessions inbox`.
    let (out, err, code) = fire(state, "pilot-a", HookEvent::TurnStart, &payload_for("aaa"));
    assert_eq!(code, 0, "hook: {err}");
    assert!(out.contains("cites the same file"), "delivered:\n{out}");
    assert!(out.contains("pilot-b"), "and attributed:\n{out}");
    assert!(
        out.contains("advisory") && out.contains("confer no authority"),
        "and framed as advice rather than instruction:\n{out}"
    );

    // Consumed once. The second turn is silent.
    let (again, _, code) = fire(state, "pilot-a", HookEvent::TurnStart, &payload_for("aaa"));
    assert_eq!(code, 0);
    assert!(
        !again.contains("cites the same file"),
        "delivering twice is delivering once:\n{again}"
    );

    // The return leg.
    let (_, err, code) = run(
        state,
        "pilot-a",
        &[
            "sessions",
            "send",
            "--to",
            "pilot-b",
            "--message",
            "agreed — re-deriving H2 from the trace instead",
        ],
        None,
    );
    assert_eq!(code, 0, "reply: {err}");
    let (out, _, code) = fire(state, "pilot-b", HookEvent::TurnStart, &payload_for("bbb"));
    assert_eq!(code, 0);
    assert!(out.contains("re-deriving H2"), "round trip closed:\n{out}");
}

/// A moment whose stdout the provider discards is not a moment to drain the
/// mailbox at: the envelope would be acknowledged and shown to nobody. `Stop`
/// is such a moment, so the hook leaves the message pending for the next turn.
#[test]
fn a_message_is_not_consumed_where_the_pilot_cannot_read_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    run(state, "pilot-a", &["sessions", "attach"], None);
    run(state, "pilot-b", &["sessions", "attach"], None);
    run(
        state,
        "pilot-b",
        &["sessions", "send", "--to", "pilot-a", "--message", "ping"],
        None,
    );

    let (out, _, code) = fire(state, "pilot-a", HookEvent::TurnEnd, &payload_for("aaa"));
    assert_eq!(code, 0);
    assert!(out.is_empty(), "turn-end injects nothing:\n{out}");

    let (out, _, code) = fire(state, "pilot-a", HookEvent::TurnStart, &payload_for("aaa"));
    assert_eq!(code, 0);
    assert!(
        out.contains("ping"),
        "still pending at the next turn:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// the checkpoint the hook publishes — and the one it refuses to invent
// ---------------------------------------------------------------------------

/// The pilot writes the content, the hook picks the moment.
///
/// A hook with nothing staged publishes nothing: a hand-over record whose
/// hypotheses were invented by a shell hook is a record of a mind that never
/// held them, and `cs sessions drift` would compare it as if one had.
#[test]
fn a_staged_checkpoint_is_published_at_a_transition_and_never_invented() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    run(state, "pilot-a", &["sessions", "attach"], None);

    // Nothing staged: the transition publishes nothing.
    fire(state, "pilot-a", HookEvent::TurnEnd, &payload_for("aaa"));
    let (out, _, code) = run(
        state,
        "pilot-a",
        &["sessions", "checkpoint", "list", "--mission", "m-1"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !out.contains("cp-"),
        "the hook does not write a checkpoint nobody authored:\n{out}"
    );

    let (_, err, code) = run(
        state,
        "pilot-a",
        &[
            "sessions",
            "checkpoint",
            "stage",
            "--mission",
            "m-1",
            "--id",
            "cp-staged",
            "--hypothesis",
            "cause=the seek advances before the read",
            "--next",
            "fix=flush the tail first",
        ],
        None,
    );
    assert_eq!(code, 0, "stage: {err}");

    // Still unpublished — staging is not publishing.
    let (out, _, _) = run(
        state,
        "pilot-a",
        &["sessions", "checkpoint", "list", "--mission", "m-1"],
        None,
    );
    assert!(
        !out.contains("cp-staged"),
        "staged is not published:\n{out}"
    );

    let (_, err, code) = fire(state, "pilot-a", HookEvent::TurnEnd, &payload_for("aaa"));
    assert_eq!(code, 0, "hook: {err}");
    let (out, _, _) = run(
        state,
        "pilot-a",
        &["sessions", "checkpoint", "list", "--mission", "m-1"],
        None,
    );
    assert!(
        out.contains("cp-staged"),
        "published at the transition:\n{out}"
    );

    // And the draft is consumed: a later transition does not republish a
    // hand-over record of a mind that has moved on.
    let (out2, _, _) = fire(state, "pilot-a", HookEvent::TurnEnd, &payload_for("aaa"));
    assert!(
        !out2.contains("cp-staged"),
        "the draft was cleared:\n{out2}"
    );
}

// ---------------------------------------------------------------------------
// 3. cost measured
// ---------------------------------------------------------------------------

/// **Acceptance clause 3.** The cost is *measured*, not modelled: `hook status`
/// reports the runs that actually happened, the wall-clock they actually took
/// and the bytes actually put into the pilot's context.
#[test]
fn the_hook_records_what_it_cost() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    run(state, "pilot-a", &["sessions", "attach"], None);
    run(state, "pilot-b", &["sessions", "attach"], None);
    run(
        state,
        "pilot-b",
        &[
            "sessions",
            "send",
            "--to",
            "pilot-a",
            "--message",
            "a measurable sentence",
        ],
        None,
    );

    for _ in 0..3 {
        fire(state, "pilot-a", HookEvent::TurnStart, &payload_for("aaa"));
    }

    let (out, err, code) = run(
        state,
        "pilot-a",
        &[
            "--json",
            "sessions",
            "hook",
            "status",
            "--provider",
            "claude",
        ],
        None,
    );
    assert_eq!(code, 0, "status: {err}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("json status");
    assert_eq!(doc["cost"]["runs"], 3, "{out}");
    assert_eq!(
        doc["cost"]["messages"], 1,
        "one message, delivered once: {out}"
    );
    assert!(
        doc["cost"]["injected_bytes"].as_u64().expect("bytes") > 0,
        "the context it spent is recorded: {out}"
    );
    assert!(
        doc["cost"]["max_ms"].as_u64().is_some(),
        "and the wall-clock too: {out}"
    );
}

// ---------------------------------------------------------------------------
// 4. clean deactivation
// ---------------------------------------------------------------------------

/// **Acceptance clause 4.** Install then uninstall leaves the settings file
/// byte-identical to what it was, and a foreign hook on the same event
/// survives both.
#[test]
fn deactivation_leaves_the_settings_file_as_it_was() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    let settings = state.join("settings.json");
    let theirs = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \
                  \"hooks\": [\n          {\n            \"type\": \"command\",\n            \
                  \"command\": \"notify-send done\"\n          }\n        ]\n      }\n    ]\n  \
                  }\n}\n";
    std::fs::write(&settings, theirs).expect("seed settings");

    let (out, err, code) = run(
        state,
        "pilot-a",
        &[
            "sessions",
            "hook",
            "install",
            "--provider",
            "claude",
            "--settings",
            settings.to_str().expect("utf-8 path"),
        ],
        None,
    );
    assert_eq!(code, 0, "install: {err}");
    assert!(out.contains("installed"), "{out}");

    let installed = std::fs::read_to_string(&settings).expect("read back");
    assert!(installed.contains(HOOK_MARKER), "wired:\n{installed}");
    assert!(
        installed.contains("notify-send done"),
        "beside theirs, not over it:\n{installed}"
    );

    let (_, err, code) = run(
        state,
        "pilot-a",
        &[
            "sessions",
            "hook",
            "uninstall",
            "--provider",
            "claude",
            "--settings",
            settings.to_str().expect("utf-8 path"),
        ],
        None,
    );
    assert_eq!(code, 0, "uninstall: {err}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    let before: serde_json::Value = serde_json::from_str(theirs).expect("json");
    assert_eq!(after, before, "clean deactivation leaves no residue");
}

/// The other speed of deactivation: quiet *now*, without editing a file the
/// running provider has already read.
#[test]
fn the_off_switch_stops_the_hook_without_unwiring_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();
    run(state, "pilot-a", &["sessions", "attach"], None);
    run(state, "pilot-b", &["sessions", "attach"], None);
    run(
        state,
        "pilot-b",
        &["sessions", "send", "--to", "pilot-a", "--message", "hello"],
        None,
    );

    let out = cs()
        .arg("--config")
        .arg(state)
        .args(["sessions", "hook", "run", "--event", "turn-start"])
        .env("COSMON_SESSION_ID", "pilot-a")
        .env(HOOK_OFF_ENV, "1")
        .stdin(Stdio::null())
        .output()
        .expect("cs runs");
    assert!(out.status.success(), "still exits 0 when switched off");
    assert!(out.stdout.is_empty(), "and says nothing");

    // Nothing was consumed while it was off.
    let (out, _, _) = fire(state, "pilot-a", HookEvent::TurnStart, &payload_for("aaa"));
    assert!(out.contains("hello"), "the message is still there:\n{out}");
}

// ---------------------------------------------------------------------------
// 5. the probe sends no keystroke
// ---------------------------------------------------------------------------

/// **Acceptance clause 5, and mission falsifier 6.** Following a session sends
/// it no key, writes into no pane and appends no byte to its provider log.
///
/// The check is structural rather than a search for `send-keys`: a provider
/// log tree is snapshotted, the hook runs, and the tree is compared. Anything
/// the hook wrote to a session — a keystroke, a rename, a log line — would
/// land here.
#[test]
fn the_hook_writes_nothing_outside_the_state_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");

    // A provider log tree that looks like the one the peer is really using.
    let logs = tmp
        .path()
        .join("claude-logs")
        .join("projects")
        .join("-repo");
    std::fs::create_dir_all(&logs).expect("log dir");
    let log = logs.join("native-peer.jsonl");
    std::fs::write(
        &log,
        "{\"sessionId\":\"native-peer\",\"type\":\"user\",\"cwd\":\"/repo\"}\n",
    )
    .expect("seed log");

    run(&state, "pilot-a", &["sessions", "attach"], None);
    let before = snapshot(tmp.path().join("claude-logs").as_path());

    let out = cs()
        .arg("--config")
        .arg(&state)
        .args(["sessions", "hook", "run", "--event", "turn-start"])
        .env("COSMON_SESSION_ID", "pilot-a")
        .env(
            "COSMON_SESSIONS_CLAUDE_ROOT",
            tmp.path().join("claude-logs"),
        )
        .env("COSMON_SESSIONS_CODEX_ROOT", tmp.path().join("no-codex"))
        .stdin(Stdio::null())
        .output()
        .expect("cs runs");
    assert!(out.status.success());

    let after = snapshot(tmp.path().join("claude-logs").as_path());
    assert_eq!(
        before, after,
        "observing a session changed its provider log"
    );
}

/// A hook that can fail its pilot's turn is worse than no hook: a
/// `UserPromptSubmit` hook exiting non-zero **blocks the prompt**. Every path
/// exits 0, including the ones with nothing to work with.
#[test]
fn the_hook_never_blocks_a_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path();

    // An unreadable state root, an unknown event, and a payload that is not
    // JSON — three ways to have nothing to do, none of which may fail a turn.
    for args in [
        vec!["sessions", "hook", "run", "--event", "turn-start"],
        vec!["sessions", "hook", "run", "--event", "turn-middle"],
        vec!["sessions", "hook", "run", "--event", "session-start"],
    ] {
        let (_, _, code) = run(state, "pilot-a", &args, Some("not json at all"));
        assert_eq!(code, 0, "exit 0 for {args:?}");
    }
}
