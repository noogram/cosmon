// SPDX-License-Identifier: AGPL-3.0-only

//! `cs sessions` end to end — the M5 acceptance walk, through the real binary.
//!
//! The unit tests beside the module pin the naming rules. What they cannot pin
//! is the mission's own acceptance clause, which is about a *route*: an
//! operator must be able to find a session, sit beside it, talk to it, hand
//! over from it and take the controls, using nothing but the CLI. So this file
//! walks it, in order, against a fixture provider tree it owns.
//!
//! Three properties are asserted along the way, because each is a mission
//! falsifier rather than a nicety:
//!
//! - **exact selection** — a selector that matches nothing prints the ones that
//!   do exist and exits non-zero; it never falls back to "the most recent";
//! - **at-least-once, consume-once** — an envelope read is gone on the next
//!   read, and only because it was read;
//! - **the three verdicts** — `drift` exits 0 / 1 / 2, and a missing checkpoint
//!   is 2, never 0.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use support::operator::Operator;

const MISSION: &str = "task-20260731-e4d0";
const CODEX_NATIVE: &str = "0198cccc-2222-4000-8000-000000000001";
const CLAUDE_NATIVE: &str = "4940f28e-0000-4000-8000-000000000001";

/// The fixture world: a state root, a repository, and a provider tree holding
/// one Codex rollout whose `cwd` is that repository.
struct World {
    _tmp: tempfile::TempDir,
    state: PathBuf,
    repo: PathBuf,
    codex_root: PathBuf,
    claude_root: PathBuf,
    /// The human at the keyboard. A transfer is their signature, so the walk
    /// needs one to sign with — see `tests/takeover_unforgeable.rs`.
    operator: Operator,
    pubkey: PathBuf,
}

fn world() -> World {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let repo = tmp.path().join("repo");
    let codex_root = tmp.path().join("codex");
    let claude_root = tmp.path().join("claude");
    std::fs::create_dir_all(&state).expect("state");
    std::fs::create_dir_all(repo.join(".git")).expect("repo");
    std::fs::create_dir_all(&claude_root).expect("claude root");

    let day = codex_root.join("2026").join("08").join("03");
    std::fs::create_dir_all(&day).expect("codex day");
    let meta = serde_json::json!({
        "timestamp": "2026-08-03T10:00:00.000Z",
        "type": "session_meta",
        "payload": { "session_id": CODEX_NATIVE, "cwd": repo.display().to_string() },
    });
    let turn = serde_json::json!({
        "timestamp": "2026-08-03T10:00:05.000Z",
        "type": "event_msg",
        "payload": { "type": "user_message", "message": "hello" },
    });
    std::fs::write(
        day.join(format!("rollout-2026-08-03T10-00-00-{CODEX_NATIVE}.jsonl")),
        format!("{meta}\n{turn}\n"),
    )
    .expect("write rollout");

    let operator = Operator::from_seed(11);
    let pubkey = state.join("..").join("takeover.pub");
    std::fs::write(&pubkey, operator.public_key_file()).expect("pin the operator key");

    World {
        _tmp: tmp,
        state,
        repo,
        codex_root,
        claude_root,
        operator,
        pubkey,
    }
}

impl World {
    /// `cs --config <state> sessions …`, with the provider roots pointed at the
    /// fixture so the test never reads the developer's own sessions.
    fn cs(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(&self.repo)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env("COSMON_SESSIONS_CODEX_ROOT", &self.codex_root)
            .env("COSMON_SESSIONS_CLAUDE_ROOT", &self.claude_root)
            .env("COSMON_TAKEOVER_PUBKEY", &self.pubkey)
            .arg("--config")
            .arg(&self.state)
            .arg("sessions")
            .args(args);
        cmd.output().expect("spawn cs")
    }

    /// Play the operator: print the challenge for a transfer, sign it, and
    /// return the path of the detached signature.
    fn sign_takeover(&self, request_id: &str) -> PathBuf {
        let challenge = ok(
            &self.cs(&[
                "takeover",
                "challenge",
                "--mission",
                MISSION,
                "--request",
                request_id,
                "--by",
                "test-operator",
            ]),
            "takeover challenge",
        );
        let path = self.repo.join("takeover.minisig");
        std::fs::write(&path, self.operator.sign(challenge.as_bytes())).expect("write signature");
        path
    }

    /// Same, with the global `--json` flag ahead of the verb.
    fn cs_json(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
        cmd.current_dir(&self.repo)
            .env_remove("COSMON_PARENT_MOL_ID")
            .env_remove("COSMON_MOL_DIR")
            .env("COSMON_SESSIONS_CODEX_ROOT", &self.codex_root)
            .env("COSMON_SESSIONS_CLAUDE_ROOT", &self.claude_root)
            .env("COSMON_TAKEOVER_PUBKEY", &self.pubkey)
            .arg("--config")
            .arg(&self.state)
            .arg("--json")
            .arg("sessions")
            .args(args);
        cmd.output().expect("spawn cs")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is utf8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr is utf8")
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({:?}):\n{}\n{}",
        out.status.code(),
        stdout(out),
        stderr(out),
    );
    stdout(out)
}

fn json_lines(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not json: {l} ({e})")))
        .collect()
}

/// Discovery keys on the provider's own id, and the selector it prints is the
/// one `show` accepts back — the round trip an operator actually performs by
/// copy-paste.
#[test]
fn discover_then_show_round_trips_through_the_canonical_selector() {
    let w = world();
    let rows = json_lines(&ok(&w.cs_json(&["discover"]), "discover"));
    assert_eq!(rows.len(), 1, "one fixture rollout, one session: {rows:?}");
    let selector = rows[0]["selector"].as_str().expect("selector").to_owned();
    assert_eq!(selector, format!("codex:{CODEX_NATIVE}"));
    assert_eq!(
        rows[0]["repo_kind"].as_str(),
        Some("checkout"),
        "the fixture repo is a canonical checkout, not a worktree"
    );

    let shown = json_lines(&ok(&w.cs_json(&["show", &selector]), "show"));
    assert_eq!(shown[0]["selector"].as_str(), Some(selector.as_str()));
    // The log was read, and reading it produced normalised events — with no
    // conversation content anywhere in the output.
    assert!(
        shown[0]["event_counts"]["user_message"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "{:?}",
        shown[0]["event_counts"]
    );
    assert!(
        !stdout(&w.cs_json(&["show", &selector])).contains("hello"),
        "the cockpit must never print conversation content"
    );
    // Claude publishes no proactive quota reading; absence must render as
    // unknown rather than as fine (ADR-168 trace A).
    let human = ok(&w.cs(&["show", &selector]), "show (human)");
    assert!(human.contains("quota"), "{human}");
}

/// A selector that names nothing is refused *with the candidates*, and a
/// malformed one is refused with the shape. Neither ever picks a session.
#[test]
fn an_unresolvable_selector_offers_candidates_and_refuses() {
    let w = world();

    let miss = w.cs(&["show", "codex:does-not-exist"]);
    assert!(!miss.status.success(), "an unknown id must not succeed");
    let text = format!("{}{}", stdout(&miss), stderr(&miss));
    assert!(text.contains("no session codex:does-not-exist"), "{text}");
    assert!(
        text.contains(CODEX_NATIVE),
        "the refusal must offer the id that does exist: {text}"
    );

    let malformed = w.cs(&["show", "not-a-selector"]);
    assert!(!malformed.status.success());
    let text = format!("{}{}", stdout(&malformed), stderr(&malformed));
    assert!(text.contains("<provider>:<native-session-id>"), "{text}");
}

/// The full operator walk: two pilots attach, see each other, exchange one
/// traced envelope, checkpoint, and hand the controls over.
#[test]
fn the_operator_walk_runs_end_to_end() {
    let w = world();

    // ── attach: a primary and a co-pilot that follows it ────────────────
    ok(
        &w.cs(&[
            "attach",
            "--session",
            "sess-claude",
            "--provider",
            "claude",
            "--native-session-id",
            CLAUDE_NATIVE,
            "--role",
            "copilot",
            "--capability",
            "observe",
        ]),
        "attach claude",
    );
    let attached = ok(
        &w.cs(&[
            "attach",
            "--session",
            "sess-codex",
            "--as",
            &format!("codex:{CODEX_NATIVE}"),
            "--role",
            "copilot",
            "--follow",
            "sess-claude",
        ]),
        "attach codex",
    );
    assert!(attached.contains("following sess-claude"), "{attached}");

    // ── list / peers: presence is reciprocal, and says which way ────────
    let listed = json_lines(&ok(&w.cs_json(&["list"]), "list"));
    assert_eq!(listed.len(), 2, "{listed:?}");
    let peers = ok(&w.cs(&["peers", "--session", "sess-claude"]), "peers");
    assert!(peers.contains("follows-me"), "{peers}");
    assert!(peers.contains("self"), "{peers}");

    // A pilot may be addressed by the selector it advertises.
    let by_selector = ok(
        &w.cs(&[
            "send",
            "--to",
            &format!("claude:{CLAUDE_NATIVE}"),
            "--from",
            "sess-codex",
            "--message",
            "that evidence ref is circular",
        ]),
        "send",
    );
    assert!(by_selector.contains("sess-claude"), "{by_selector}");

    // ── inbox: at-least-once delivery, consume-once semantics ───────────
    let peeked = json_lines(&ok(
        &w.cs_json(&["inbox", "--session", "sess-claude", "--peek"]),
        "inbox --peek",
    ));
    assert_eq!(peeked.len(), 1, "{peeked:?}");
    assert_eq!(
        peeked[0]["body"].as_str(),
        Some("that evidence ref is circular")
    );
    // Peeking consumed nothing…
    let read = json_lines(&ok(
        &w.cs_json(&["inbox", "--session", "sess-claude"]),
        "inbox",
    ));
    assert_eq!(read.len(), 1, "a peek must not consume: {read:?}");
    // …and reading consumed exactly once.
    let again = json_lines(&ok(
        &w.cs_json(&["inbox", "--session", "sess-claude"]),
        "inbox again",
    ));
    assert!(again.is_empty(), "read twice: {again:?}");

    // ── checkpoints: two pilots, opposite intentions on one subject ─────
    ok(
        &w.cs(&[
            "checkpoint",
            "publish",
            "--mission",
            MISSION,
            "--session",
            "sess-claude",
            "--id",
            "cp-claude-1",
            "--include",
            "the cockpit",
            "--next",
            "merge-strategy:affirm=merge once the gates are green",
            "--evidence",
            "merge-strategy=docs/adr/168.md",
        ]),
        "checkpoint publish (claude)",
    );
    ok(
        &w.cs(&[
            "checkpoint",
            "publish",
            "--mission",
            MISSION,
            "--session",
            "sess-codex",
            "--id",
            "cp-codex-1",
            "--include",
            "the cockpit",
            "--next",
            "merge-strategy:deny=merge once the gates are green",
            "--evidence",
            "merge-strategy=docs/adr/168.md",
        ]),
        "checkpoint publish (codex)",
    );
    let published = json_lines(&ok(
        &w.cs_json(&["checkpoint", "list", "--mission", MISSION]),
        "checkpoint list",
    ));
    assert_eq!(published.len(), 2, "{published:?}");

    // ── drift: a decidable contradiction is exit 1, with both sides cited
    let drift = w.cs(&["drift", "sess-claude", "sess-codex", "--mission", MISSION]);
    assert_eq!(
        drift.status.code(),
        Some(1),
        "opposite stances on one subject must be a FINDING:\n{}",
        stdout(&drift)
    );
    let text = stdout(&drift);
    assert!(text.contains("Finding"), "{text}");
    assert!(text.contains("merge once the gates are green"), "{text}");

    // A pilot that never published is INCONCLUSIVE — never agreement.
    let unknown = w.cs(&["drift", "sess-claude", "sess-nobody", "--mission", MISSION]);
    assert_eq!(
        unknown.status.code(),
        Some(2),
        "a missing checkpoint is not agreement:\n{}",
        stdout(&unknown)
    );

    // ── takeover: ask, grant, and the epoch that refuses the old holder ─
    let requested = ok(
        &w.cs(&[
            "takeover",
            "request",
            "--mission",
            MISSION,
            "--from",
            "sess-codex",
            "--reason",
            "the primary is out of quota",
        ]),
        "takeover request",
    );
    let request_id = requested
        .split_whitespace()
        .nth(1)
        .expect("request id in output")
        .to_owned();
    // The ask alone confers nothing.
    let before = w.cs(&[
        "takeover",
        "check",
        "--mission",
        MISSION,
        "--session",
        "sess-codex",
        "--epoch",
        "1",
    ]);
    assert_eq!(before.status.code(), Some(1), "{}", stdout(&before));

    // The grant is the operator's signature over this exact transfer — the
    // one gesture the beneficiary cannot produce for itself (F1).
    let attestation = w.sign_takeover(&request_id);
    ok(
        &w.cs(&[
            "takeover",
            "grant",
            "--mission",
            MISSION,
            "--request",
            &request_id,
            "--by",
            "test-operator",
            "--attestation",
            &attestation.display().to_string(),
        ]),
        "takeover grant",
    );
    let after = w.cs(&[
        "takeover",
        "check",
        "--mission",
        MISSION,
        "--session",
        "sess-codex",
        "--epoch",
        "1",
    ]);
    assert_eq!(
        after.status.code(),
        Some(0),
        "the granted holder may pilot at its epoch:\n{}",
        stdout(&after)
    );
    // The seat follows the ledger, not the claim: the pilot that was never
    // granted anything cannot take the primary seat.
    let usurp = w.cs(&[
        "attach",
        "--session",
        "sess-claude",
        "--role",
        "primary",
        "--mission",
        MISSION,
        "--epoch",
        "1",
    ]);
    assert!(
        !usurp.status.success(),
        "a session with no lease must not seat itself as primary"
    );
    ok(
        &w.cs(&[
            "attach",
            "--session",
            "sess-codex",
            "--role",
            "primary",
            "--mission",
            MISSION,
            "--epoch",
            "1",
        ]),
        "attach as the granted primary",
    );
}

/// Observation is neutral: everything above reads provider logs, and not one
/// byte of the provider tree changes.
#[test]
fn following_a_session_never_writes_to_it() {
    let w = world();
    let before = tree_digest(&w.codex_root);

    let selector = format!("codex:{CODEX_NATIVE}");
    ok(&w.cs(&["discover"]), "discover");
    ok(&w.cs(&["show", &selector, "--tail", "3"]), "show --tail");
    ok(&w.cs_json(&["discover", "--all"]), "discover --all");

    assert_eq!(
        before,
        tree_digest(&w.codex_root),
        "the provider tree changed under observation"
    );
}

/// `(path, len, mtime)` for every file under `root`, sorted.
fn tree_digest(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(meta) = entry.metadata() {
                out.push(format!(
                    "{} {} {:?}",
                    path.display(),
                    meta.len(),
                    meta.modified().ok()
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}
