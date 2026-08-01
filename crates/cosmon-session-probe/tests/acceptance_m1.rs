// SPDX-License-Identifier: Apache-2.0

//! The M1 acceptance clauses, one test each.
//!
//! The mission states them as: *"fixtures without secrets; resumption by
//! cursor; truncated file, rotation, session without a name, two sessions in
//! the same cwd and homonym worktree tested; no observation modifies the
//! provider logs."* Each is below under a name that says which clause it is,
//! so a failure names the invariant rather than a function.

use std::path::{Path, PathBuf};

use cosmon_session_probe::{
    ClaudeProbe, CodexProbe, Continuity, Cursor, DiscoveryFilter, NativeSessionId, ProbeRegistry,
    ProviderName, ProviderSessionRef, RawLine, RepoIdentity, RestartCause, SessionEvent,
    SessionEventKind, SessionProbe, SessionSelector,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn claude_probe() -> ClaudeProbe {
    ClaudeProbe::new(fixtures().join("claude").join("projects")).unwrap()
}

fn codex_probe() -> CodexProbe {
    CodexProbe::new(fixtures().join("codex").join("sessions")).unwrap()
}

/// Every fixture file, recursively.
fn fixture_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&fixtures(), &mut out);
    out.sort();
    out
}

// ── Clause: fixtures without secrets ────────────────────────────────────────

/// A fixture pasted in from a real session must fail the suite, not the review.
///
/// The shapes below are the ones a captured agent log actually carries: API
/// keys, bearer tokens, JWTs, PEM blocks, e-mail addresses, and — the one that
/// gets pasted in without anyone noticing — a real `$HOME` path.
#[test]
fn clause_fixtures_carry_no_secret_and_no_real_path() {
    const FORBIDDEN: &[(&str, &str)] = &[
        ("sk-", "an API key prefix"),
        ("ghp_", "a GitHub token prefix"),
        ("AKIA", "an AWS access key id"),
        ("-----BEGIN", "a PEM block"),
        ("Bearer ", "an authorization header"),
        ("eyJ", "a JWT"),
        ("@", "an e-mail address or a host@ reference"),
        ("/Users/", "a macOS home path"),
        ("/home/", "a Linux home path"),
        ("/private/", "a macOS temp/real path"),
    ];

    let logs: Vec<_> = fixture_files()
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert!(!logs.is_empty(), "there are fixtures to scan");

    for file in logs {
        let text = std::fs::read_to_string(&file).unwrap();
        for (needle, what) in FORBIDDEN {
            assert!(
                !text.contains(needle),
                "{} contains {needle:?} — {what}. Fixtures are written from the \
                 ADR-168 record-type histograms, never captured and redacted.",
                file.display()
            );
        }
    }
}

/// The confidentiality ceiling, expressed as an assertion rather than a
/// promise: the normalised events of a whole fixture session carry sizes and
/// counters, and the message bodies never appear in them.
#[test]
fn clause_no_conversation_content_crosses_into_an_event() {
    let probe = claude_probe();
    let sessions = probe.discover(&DiscoveryFilter::all()).unwrap();
    let read = probe.read(&sessions[0], Cursor::start()).unwrap();

    let rendered = serde_json::to_string(&read.events).unwrap();
    assert!(
        !rendered.contains("<fixture>"),
        "the fixture message body reached a normalised event: {rendered}"
    );
    assert!(
        read.events
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::UserMessage { chars } if chars > 0)),
        "the size of the message is still reported"
    );
}

// ── Clause: resumption by cursor ────────────────────────────────────────────

/// Poll, get everything; poll again with the returned cursor, get nothing;
/// append, poll again, get exactly what was appended. No loss, no duplicate.
#[test]
fn clause_a_cursor_resumes_exactly_where_it_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());

    let first = probe.read(&session, Cursor::start()).unwrap();
    assert_eq!(first.continuity, Continuity::Fresh);
    let seen = first.events.len();
    assert!(seen >= 5);

    let idle = probe.read(&session, first.cursor).unwrap();
    assert_eq!(idle.continuity, Continuity::Resumed);
    assert!(idle.events.is_empty(), "a quiet session yields no events");

    append(
        &session.source_locator,
        r#"{"type":"user","sessionId":"00000000-0000-4000-8000-000000000001","timestamp":"2026-08-01T00:10:00.000Z","message":{"role":"user","content":"<fixture>"}}"#,
    );
    let next = probe.read(&session, idle.cursor).unwrap();
    assert_eq!(next.continuity, Continuity::Resumed);
    assert_eq!(next.events.len(), 1, "exactly the new line, once");
    assert!(matches!(
        next.events[0].kind,
        SessionEventKind::UserMessage { .. }
    ));
}

/// A cursor survives a process restart, which is the whole point of it being a
/// value rather than an open file handle.
#[test]
fn clause_a_cursor_survives_being_persisted_and_read_back() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());

    let first = probe.read(&session, Cursor::start()).unwrap();
    let json = serde_json::to_string(&first.cursor).unwrap();
    let restored: Cursor = serde_json::from_str(&json).unwrap();

    append(
        &session.source_locator,
        r#"{"type":"mode","mode":"default"}"#,
    );
    let next = probe.read(&session, restored).unwrap();
    assert_eq!(next.continuity, Continuity::Resumed);
    assert_eq!(next.events.len(), 1);
}

// ── Clause: truncated file ──────────────────────────────────────────────────

/// A live log sampled mid-append must not lose the line, and must not deliver
/// half of it. Probe P7: `claudion::parse_session` errors on the whole file
/// here.
#[test]
fn clause_a_half_written_line_is_delivered_once_when_it_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());
    let first = probe.read(&session, Cursor::start()).unwrap();

    append_raw(
        &session.source_locator,
        r#"{"type":"assistant","message":{"model":"fixture-model-1","usage":{"input_tokens":1"#,
    );
    let mid = probe.read(&session, first.cursor).unwrap();
    assert!(mid.events.is_empty(), "no half line is delivered");
    assert!(mid.pending_bytes > 0, "and the port says it is waiting");

    append_raw(&session.source_locator, "}}}\n");
    let complete = probe.read(&session, mid.cursor).unwrap();
    assert_eq!(complete.events.len(), 1);
    assert!(matches!(
        complete.events[0].kind,
        SessionEventKind::AssistantMessage { .. }
    ));
}

/// Probe P4, as a test: a shrunken file must not leave the reader reporting
/// success while reading nothing forever.
#[test]
fn clause_a_truncated_log_rewinds_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());
    let first = probe.read(&session, Cursor::start()).unwrap();
    assert!(!first.events.is_empty());

    std::fs::write(
        &session.source_locator,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<fixture>\"}}\n",
    )
    .unwrap();

    let after = probe.read(&session, first.cursor).unwrap();
    assert_eq!(
        after.continuity,
        Continuity::Restarted(RestartCause::Truncated),
        "silence would have been the P4 bug"
    );
    assert_eq!(after.events.len(), 1);
}

// ── Clause: rotation ────────────────────────────────────────────────────────

/// Rotation to a file of the *same length* is the case a length check cannot
/// see — and the case a rotating writer produces most easily.
#[test]
fn clause_a_rotated_log_of_equal_length_is_still_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());
    let first = probe.read(&session, Cursor::start()).unwrap();

    let old = std::fs::read_to_string(&session.source_locator).unwrap();
    let rotated = old.replace("0000000000a1", "0000000000f9");
    assert_eq!(rotated.len(), old.len(), "same byte length by construction");
    std::fs::write(&session.source_locator, &rotated).unwrap();

    let after = probe.read(&session, first.cursor).unwrap();
    assert_eq!(
        after.continuity,
        Continuity::Restarted(RestartCause::Rotated)
    );
    assert!(!after.events.is_empty(), "the backlog is not swallowed");
}

/// Probe P5, as a test: a stale cursor landing inside a multi-byte character
/// must not panic the reading process.
#[test]
fn clause_a_stale_cursor_inside_a_codepoint_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let (probe, session) = live_claude_session(tmp.path());

    let mut bytes = vec![b'a'; 113];
    bytes.extend("é".as_bytes());
    bytes.extend(b"\n{\"type\":\"mode\",\"mode\":\"default\"}\n");
    std::fs::write(&session.source_locator, bytes).unwrap();

    let read = probe.read(&session, Cursor::from_offset(114)).unwrap();
    assert!(read
        .events
        .iter()
        .any(|e| matches!(&e.kind, SessionEventKind::Other { record } if record == "mode")));
}

// ── Clause: a session without a name ────────────────────────────────────────

/// Mission falsifier: *"two unnamed sessions in the same repo are confused"*.
/// A name is an alias; the key is the native id, so a nameless session is
/// addressed exactly like a named one.
#[test]
fn clause_an_unnamed_session_is_addressed_exactly_like_a_named_one() {
    let sessions = claude_probe().discover(&DiscoveryFilter::all()).unwrap();

    let unnamed = sessions
        .iter()
        .find(|s| s.display_name.is_none())
        .expect("the fixture set contains a session with no title");
    let named = sessions
        .iter()
        .find(|s| s.display_name.is_some())
        .expect("and one with a title");

    for s in [unnamed, named] {
        let selector = s.selector();
        assert_eq!(
            selector.to_string().parse::<SessionSelector>().unwrap(),
            selector,
            "both round-trip through the same canonical selector"
        );
    }
    assert_ne!(unnamed.selector(), named.selector());

    // And the name is not part of the key: erasing it changes nothing.
    let mut stripped = named.clone();
    stripped.display_name = None;
    assert_eq!(stripped.selector(), named.selector());
}

// ── Clause: two sessions in the same cwd ────────────────────────────────────

/// `resolve_codex_session_by_cwd` returns the most-recently-modified match and
/// so collapses these two into one (probe P6). Discovery returns the set.
#[test]
fn clause_two_sessions_in_one_cwd_stay_two_sessions() {
    for (label, found) in [
        (
            "claude",
            claude_probe().discover(&DiscoveryFilter::all()).unwrap(),
        ),
        (
            "codex",
            codex_probe().discover(&DiscoveryFilter::all()).unwrap(),
        ),
    ] {
        let in_fixture_cwd: Vec<_> = found
            .iter()
            .filter(|s| s.cwd.as_deref() == Some(Path::new("/fixture/galaxy")))
            .collect();
        assert_eq!(
            in_fixture_cwd.len(),
            2,
            "{label}: both sessions of the shared cwd are returned"
        );

        let mut ids: Vec<_> = in_fixture_cwd
            .iter()
            .map(|s| s.native_session_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 2, "{label}: and they are distinguishable");
    }
}

// ── Clause: homonym worktree ────────────────────────────────────────────────

/// Mission falsifier: *"a worktree named like the root is chosen by
/// substring"*. Three checkouts whose paths all contain `galaxy`; a filter for
/// one returns one.
#[test]
fn clause_a_homonym_worktree_is_never_selected_for_the_canonical_root() {
    let tmp = tempfile::tempdir().unwrap();

    let root = tmp.path().join("galaxy");
    std::fs::create_dir_all(root.join(".git").join("worktrees").join("task-a")).unwrap();

    // A linked worktree of `galaxy` …
    let worktree = root.join(".worktrees").join("task-a");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!(
            "gitdir: {}\n",
            root.join(".git").join("worktrees").join("task-a").display()
        ),
    )
    .unwrap();

    // … and an unrelated repository whose name merely starts the same way.
    let homonym = tmp.path().join("galaxy-scratch");
    std::fs::create_dir_all(homonym.join(".git")).unwrap();

    let projects = tmp.path().join("projects");
    for (i, cwd) in [&root, &worktree, &homonym].iter().enumerate() {
        write_claude_log(
            &projects,
            &format!("00000000-0000-4000-8000-00000000010{i}"),
            cwd,
        );
    }
    let probe = ClaudeProbe::new(&projects).unwrap();

    let all = probe.discover(&DiscoveryFilter::all()).unwrap();
    assert_eq!(all.len(), 3, "all three are visible");

    for cwd in [&root, &worktree, &homonym] {
        let repo = RepoIdentity::resolve(cwd).unwrap();
        let found = probe.discover(&DiscoveryFilter::in_repo(repo)).unwrap();
        assert_eq!(
            found.len(),
            1,
            "exactly one session for {} — a substring test would return more",
            cwd.display()
        );
        assert_eq!(
            found[0]
                .cwd
                .as_ref()
                .map(|c| std::fs::canonicalize(c).unwrap()),
            Some(std::fs::canonicalize(cwd).unwrap())
        );
    }

    // And the relation between the worktree and its checkout is still legible.
    let wt = RepoIdentity::resolve(&worktree).unwrap();
    let canonical = RepoIdentity::resolve(&root).unwrap();
    assert!(!wt.is_same(&canonical));
    assert_eq!(
        wt.linked_root().map(|p| std::fs::canonicalize(p).unwrap()),
        Some(std::fs::canonicalize(&root).unwrap())
    );
}

/// Probe P6: the Claude project directory name is not invertible, so the
/// adapter must read `cwd` from inside the log. The fixture directory is named
/// after a path the logs do not use — a decoder would produce that one.
#[test]
fn clause_the_project_directory_name_is_never_decoded() {
    let sessions = claude_probe().discover(&DiscoveryFilter::all()).unwrap();
    assert!(!sessions.is_empty());
    for s in &sessions {
        assert_eq!(
            s.cwd.as_deref(),
            Some(Path::new("/fixture/galaxy")),
            "cwd comes from the record, not from the `-fixture-decoy-galaxy` directory name"
        );
    }
}

// ── Clause: no observation modifies the provider logs ───────────────────────

/// OBSERVATION-NEUTRE. Discovery and a full read of every fixture, then a
/// byte-for-byte and mtime comparison of the whole fixture tree.
#[test]
fn clause_observation_changes_no_byte_and_no_mtime() {
    fn snapshot() -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        fixture_files()
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).unwrap();
                let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
                (p, bytes, mtime)
            })
            .collect()
    }

    let before = snapshot();

    let registry = ProbeRegistry::new()
        .with(Box::new(claude_probe()))
        .with(Box::new(codex_probe()));
    for session in registry.discover(&DiscoveryFilter::all()).unwrap() {
        let probe = registry.probe_for(&session.provider).unwrap();
        let mut cursor = Cursor::start();
        // Poll it the way a co-pilot would: repeatedly, to exhaustion.
        for _ in 0..3 {
            cursor = probe.read(&session, cursor).unwrap().cursor;
        }
    }

    let after = snapshot();
    assert_eq!(
        before.len(),
        after.len(),
        "observation added or removed a file"
    );
    for ((path, bytes, mtime), (path_after, bytes_after, mtime_after)) in before.iter().zip(&after)
    {
        assert_eq!(path, path_after);
        assert_eq!(bytes, bytes_after, "observation rewrote {}", path.display());
        assert_eq!(
            mtime,
            mtime_after,
            "observation touched the mtime of {}",
            path.display()
        );
    }
}

// ── Clause: a third provider is an adapter, not a cockpit change ────────────

/// Mission falsifier 10, as a compile-and-run proof: a provider defined
/// entirely inside this test file joins the registry and is discovered and read
/// through the same calls, with no edit to any type in the crate.
#[test]
fn clause_a_third_provider_is_added_without_touching_the_port() {
    struct ToyProbe {
        provider: ProviderName,
        log: PathBuf,
    }

    impl SessionProbe for ToyProbe {
        fn provider(&self) -> &ProviderName {
            &self.provider
        }

        fn discover(
            &self,
            filter: &DiscoveryFilter,
        ) -> Result<Vec<ProviderSessionRef>, cosmon_session_probe::ProbeError> {
            let session = ProviderSessionRef {
                provider: self.provider.clone(),
                native_session_id: NativeSessionId::new("toy-1").unwrap(),
                repo_identity: None,
                cwd: None,
                source_locator: self.log.clone(),
                display_name: None,
                started_at: None,
                last_observed_at: None,
            };
            Ok(filter
                .accepts(&session)
                .then_some(session)
                .into_iter()
                .collect())
        }

        fn normalize(&self, line: &RawLine) -> SessionEvent {
            SessionEvent {
                offset: line.offset,
                at: None,
                kind: SessionEventKind::Other {
                    record: line.text.clone(),
                },
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("toy.log");
    std::fs::write(&log, "alpha\nbeta\n").unwrap();

    let registry = ProbeRegistry::new()
        .with(Box::new(claude_probe()))
        .with(Box::new(ToyProbe {
            provider: ProviderName::new("toy").unwrap(),
            log,
        }));

    let toy = ProviderName::new("toy").unwrap();
    let sessions = registry.discover(&DiscoveryFilter::all()).unwrap();
    let toy_session = sessions
        .iter()
        .find(|s| s.provider == toy)
        .expect("the new provider is discovered through the same call");
    assert_eq!(toy_session.selector().to_string(), "toy:toy-1");

    let read = registry
        .probe_for(&toy)
        .unwrap()
        .read(toy_session, Cursor::start())
        .unwrap();
    assert_eq!(read.events.len(), 2);
    assert_eq!(
        registry.candidates(&toy_session.selector()).unwrap().len(),
        1
    );
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Copy the Claude fixture into a writable directory and return a probe over
/// it plus the discovered session. The fixtures themselves stay read-only —
/// the neutrality test compares them byte for byte.
fn live_claude_session(dir: &Path) -> (ClaudeProbe, ProviderSessionRef) {
    let projects = dir.join("projects").join("-fixture-decoy-galaxy");
    std::fs::create_dir_all(&projects).unwrap();
    let name = "00000000-0000-4000-8000-000000000001.jsonl";
    std::fs::copy(
        fixtures()
            .join("claude")
            .join("projects")
            .join("-fixture-decoy-galaxy")
            .join(name),
        projects.join(name),
    )
    .unwrap();

    let probe = ClaudeProbe::new(dir.join("projects")).unwrap();
    let session = probe
        .discover(&DiscoveryFilter::all())
        .unwrap()
        .into_iter()
        .next()
        .expect("the copied fixture is discovered");
    (probe, session)
}

/// Append one JSONL record (adding the newline).
fn append(path: &Path, line: &str) {
    append_raw(path, &format!("{line}\n"));
}

/// Append raw bytes — used to write half a line on purpose.
fn append_raw(path: &Path, text: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// Write a minimal Claude log announcing `cwd`, under a project directory whose
/// name is deliberately unrelated to it.
fn write_claude_log(projects: &Path, session_id: &str, cwd: &Path) -> PathBuf {
    let dir = projects.join("-decoy");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    let line = serde_json::json!({
        "type": "user",
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
        "gitBranch": "fixture/main",
        "timestamp": "2026-08-01T00:00:00.000Z",
        "message": {"role": "user", "content": "<fixture>"},
    });
    std::fs::write(&path, format!("{line}\n")).unwrap();
    path
}
