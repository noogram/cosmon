// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/session` — a paginated, non-streaming read of a
//! worker's **message thread**.
//!
//! # The gap this closes (issue #51 follow-up)
//!
//! [`crate::routes::logs_stream`] already streams the worker's tmux pane as
//! SSE. It is a live tail of a *screen*: no history before the connection, no
//! structure (a prompt awaiting an answer and a scrolled-off log line are the
//! same `log.line`), and nothing at all once the worker exits. The reporters
//! asked for the other thing — the structured, retrievable transcript a human
//! sees when they attach to the worker (`cs peek`, or the tmux pane): what was
//! said, by whom, in what order, readable *after* the fact and *without* a
//! live tmux, so that "this molecule is waiting for something" is visible
//! rather than inferred.
//!
//! This route is that read. It is **read-only** by construction: writing into
//! a session is explicitly deferred, and neither this module nor the
//! `cosmon-remote molecule session` verb contains a write path — pinned by
//! `tests/session_is_read_only.rs`, which greps both surfaces for `send-keys`
//! and the transport's input methods.
//!
//! # Where the thread comes from
//!
//! The words are written by the agent, not by cosmon, so the source is the
//! agent's own session log on the host — see
//! [`cosmon_core::session_thread`] for the schemas and the finding. Resolution
//! order, first hit wins:
//!
//! 1. **the pinned transcript** — the exact file
//!    [`cosmon_core::session_thread::SessionLocator`] recorded, one open and
//!    no scan at all.
//! 2. **claude transcript** — `<claude-config>/projects/{sanitised-cwd}/*.jsonl`,
//!    the most recent log for the worker's recorded working directory.
//! 3. **codex rollout** — `~/.codex/sessions/**/rollout-*.jsonl` whose
//!    `session_meta.payload.cwd` is that same directory.
//! 4. **tmux scrollback** — a capture of the live pane, when no transcript
//!    file resolves. Named as such on the wire, with its limits stated.
//! 5. **none** — nothing was retrievable. An explicit `source`, never an
//!    empty `200` that reads like an empty session.
//!
//! # The locator, and why the fleet entry was not enough
//!
//! Both transcript planes key off the worker's *working directory*, which
//! used to be recoverable only through `assigned_worker` →
//! `fleet.workers[worker].repo`. Normal teardown deletes that entry: `cs done`
//! purges the worker from `fleet.json`, and so does `cs purge`. The agent log
//! survives on disk, but nothing was left to say which directory it belonged
//! to — so the *retrospective* read this route exists for returned
//! `source: none` the moment a molecule closed cleanly, which is precisely
//! when a human wants to read it. The locator sidecar
//! (`fleets/{fleet}/molecules/{id}/session-locator.json`) is written at
//! dispatch, lives beside `result.md` and `blocked_on.json` in a directory
//! teardown keeps, and is what this route resolves from first. The fleet
//! entry remains as the fallback for molecules dispatched before it existed —
//! and their first successful read writes the sidecar, so the dependency
//! expires by itself.
//!
//! # Bounded work, not just a bounded response
//!
//! `tail`/`limit` bound what is *returned*; they used to bound nothing about
//! what was *read*. A `tail=3` request walked the host's whole codex session
//! tree, read every rollout in full to check its first eight lines, and then
//! allocated the entire selected transcript before taking three entries off
//! the end. Now: the pinned path is opened directly when there is one; a
//! candidate rollout is probed by its head alone ([`MAX_HEAD_BYTES`]); the
//! selected transcript is read from its **end** under
//! [`MAX_TRANSCRIPT_BYTES`], with `truncated` on the wire when that ceiling
//! bites; and the whole of it runs under `spawn_blocking` rather than on the
//! async executor.
//!
//! # Pipeline (same five clauses as `get_molecule` / `get_result`)
//!
//! 1. Extract bearer.
//! 2. Validate JWT.
//! 3. Scope check — `cosmon:logs:subscribe`.
//! 4. Admission boundary (`http_request_to_spark`, [`Verb::SubscribeLogs`]),
//!    so a noyau-A JWT cannot read a noyau-B session.
//! 5. Malformed `{id}` → `404 not_found` (turing §8.2.3: no existence oracle),
//!    before the id can reach a tmux argument.
//!
//! # Why `cosmon:logs:subscribe` rather than a new scope
//!
//! The thread and the pane tail are the *same data class* — the worker's own
//! output, at two fidelities. Minting `cosmon:session:read` would split one
//! class across two grants, so a deployment that had already decided "this
//! client may watch its workers work" would have to decide it a second time,
//! and the two answers could drift. Reusing the existing scope also keeps the
//! stricter posture: `cosmon:molecule:read` (which `/result` rides) is the
//! basic grant every onboarding tenant holds, and the thread is more than the
//! deliverable — it is everything the worker read and said.
//!
//! # Waiting, surfaced rather than inferred by the reader
//!
//! The response carries a `waiting` block derived from
//! [`cosmon_core::session_thread::waiting_from_entries`]: the pane classifier
//! `cs patrol --dialogue-scan` already uses
//! ([`cosmon_core::dialogue::classify_pane`]), plus the non-forgeable
//! control-plane `await-operator` signal. No marker vocabulary is invented
//! here. Per ADR-137 §2 the text half is evidence for a human only — this
//! route mutates nothing and no autonomous action is keyed off it.
//!
//! Both planes are classified, not just the one the entries came from: a live
//! permission prompt is drawn on the *screen* and need not appear in the
//! transcript at all, so a transcript-only verdict reported a worker frozen
//! at a prompt as busy. `waiting.evidence_source` names which plane fired.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use cosmon_core::id::MoleculeId;
use cosmon_core::session_thread::{
    entries_from_pane, parse_claude_transcript, parse_codex_rollout, tail, waiting_from_entries,
    window, SessionLocator, ThreadEntry, ThreadSource, WAITING_SCAN_ENTRIES,
};
use cosmon_state::MoleculeData;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admission::Verb;
use crate::auth::scopes::LOGS_SUBSCRIBE;
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::JwtVerifier;
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, observe_with_state_dir_public,
};
use crate::AppState;

/// Default page size when the caller names neither `tail` nor `limit`. Two
/// hundred entries is a few screens of thread — enough to see what a worker is
/// doing now, small enough that the default request is never the expensive
/// one.
pub const DEFAULT_LIMIT: usize = 200;

/// Ceiling on `limit` / `tail`. Combined with the per-entry cap in
/// [`cosmon_core::session_thread::MAX_ENTRY_CHARS`], it bounds the response:
/// an unbounded read route is a denial-of-service surface.
pub const MAX_LIMIT: usize = 2_000;

/// How many trailing pane lines the scrollback capture asks tmux for. Matches
/// [`crate::routes::logs_stream`]'s window so the two routes see the same
/// screen.
const CAPTURE_LINES: i32 = 4000;

/// Query parameters for `GET /v1/molecules/{id}/session`.
#[derive(Debug, Deserialize, Default)]
pub struct SessionQuery {
    /// Return only the last `N` entries. Wins over `offset`/`limit` when
    /// present — "show me the end" is the dominant read, and making it a
    /// separate parameter means a client never has to know `total` first.
    pub tail: Option<usize>,
    /// Start of the half-open window over the full thread. Ignored when
    /// `tail` is given.
    pub offset: Option<usize>,
    /// Size of the window. Clamped to [`MAX_LIMIT`].
    pub limit: Option<usize>,
}

/// The host directories the agent session logs live under.
///
/// Passed explicitly rather than read from the environment at each use so the
/// resolution is testable against a fixture tree — the same reason
/// [`crate::routes::result::resolve_canonical_result`] takes its two
/// directories as parameters.
#[derive(Debug, Clone)]
pub struct AgentSessionRoots {
    /// `<claude-config>/projects` — one subdirectory per sanitised working
    /// directory, holding that project's session logs.
    pub claude_projects: PathBuf,
    /// `~/.codex/sessions` — a date-nested tree of `rollout-*.jsonl`.
    pub codex_sessions: PathBuf,
}

impl AgentSessionRoots {
    /// Resolve from this process's environment, mirroring the precedence the
    /// agents themselves use: `CLAUDE_CONFIG_DIR` when set non-empty, else
    /// `$HOME/.claude`; `$HOME/.codex/sessions` for codex. An unset `HOME`
    /// degrades to `.`, which resolves nothing and yields
    /// [`ThreadSource::None`] — an honest miss rather than a panic.
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        let claude_config = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| PathBuf::from(&home).join(".claude"), PathBuf::from);
        Self {
            claude_projects: claude_config.join("projects"),
            codex_sessions: PathBuf::from(&home).join(".codex").join("sessions"),
        }
    }
}

/// A thread as resolved from disk (or from a pane), with the source that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedThread {
    /// Which plane the entries came from.
    pub source: ThreadSource,
    /// The full thread, ordinals assigned, before any window is applied.
    pub entries: Vec<ThreadEntry>,
}

/// Byte ceiling on a single transcript read.
///
/// A worker's own log is the one file this route is *supposed* to read, but
/// it grows without bound while the worker runs, and `read_to_string` on it
/// allocated the whole thing before any window was applied — so a `tail=3`
/// request paid for a hundred-megabyte thread. Four mebibytes is far more
/// than [`MAX_LIMIT`] entries of [`cosmon_core::session_thread::MAX_ENTRY_CHARS`]
/// could ever return, so the ceiling is invisible to every honest read and
/// present for the dishonest one. When it bites, `truncated` says so on the
/// wire rather than passing a partial thread off as a whole one.
pub const MAX_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024;

/// Byte ceiling on the *metadata* probe of a candidate codex rollout.
///
/// `session_meta` is the first line of a rollout. Deciding whether a file
/// belongs to this worker therefore needs its head, never its body — and the
/// body is what made the un-pinned scan read every historical rollout on the
/// host in full.
pub const MAX_HEAD_BYTES: u64 = 64 * 1024;

/// What one session read actually cost on the filesystem.
///
/// Returned rather than logged because it is the falsifier: "the read is
/// bounded" is a claim about opens and bytes, and a test can only hold this
/// route to it if the route reports them. The route itself does not serve
/// these numbers — they exist to be asserted against.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudget {
    /// How many files were opened and read from.
    pub files_opened: u64,
    /// How many bytes were read out of them.
    pub bytes_read: u64,
}

/// Read at most `max_bytes` from the **end** of `path`.
///
/// Reading from the end is what makes `tail` cheap: the entries a reader
/// almost always wants are the last ones, and the head of a long-running
/// worker's log is exactly the part nobody asked for. When the file is
/// larger than the ceiling the first (necessarily partial) line is dropped —
/// half a JSON object is not an entry — and `true` is returned for
/// `truncated`.
fn read_tail_bounded(
    path: &Path,
    max_bytes: u64,
    budget: &mut ReadBudget,
) -> Option<(String, bool)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    budget.files_opened += 1;
    if len <= max_bytes {
        let mut buf = String::new();
        file.read_to_string(&mut buf).ok()?;
        budget.bytes_read += buf.len() as u64;
        return Some((buf, false));
    }
    file.seek(SeekFrom::Start(len - max_bytes)).ok()?;
    let mut raw = Vec::new();
    file.take(max_bytes).read_to_end(&mut raw).ok()?;
    budget.bytes_read += raw.len() as u64;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let kept = match text.find('\n') {
        Some(nl) => text[nl + 1..].to_owned(),
        None => String::new(),
    };
    Some((kept, true))
}

/// Read at most the first `MAX_HEAD_BYTES` of `path` — the metadata probe.
fn read_head_bounded(path: &Path, budget: &mut ReadBudget) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    budget.files_opened += 1;
    let mut raw = Vec::new();
    file.take(MAX_HEAD_BYTES).read_to_end(&mut raw).ok()?;
    budget.bytes_read += raw.len() as u64;
    Some(String::from_utf8_lossy(&raw).into_owned())
}

/// The most-recently-modified `*.jsonl` directly inside `dir`.
///
/// Most recent wins because a worktree can host several attempts (a resume, a
/// retry): the log still being appended to is the current attempt's, and an
/// abandoned earlier one is at worst not shown — never mixed in.
///
/// Metadata only: no file here is opened, which is why the claude plane costs
/// one open per request no matter how many attempts a worktree has hosted.
fn most_recent_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
                best = Some((mtime, path));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Every `*.jsonl` under `root`, walked recursively (codex nests its rollouts
/// by date).
fn walk_jsonl(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_jsonl(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// The codex rollout log whose `session_meta` names `cwd`, most recent first.
///
/// Only the **head** of each candidate is read (see [`MAX_HEAD_BYTES`]), and
/// candidates are tried newest-first so the answer is normally found in the
/// first one or two files rather than after the whole tree. This scan runs
/// only when the molecule has no pinned transcript path yet; once one is
/// pinned in the [`cosmon_core::session_thread::SessionLocator`], later reads
/// open exactly that file.
fn codex_log_for_cwd(root: &Path, cwd: &str, budget: &mut ReadBudget) -> Option<PathBuf> {
    let mut files = Vec::new();
    walk_jsonl(root, &mut files);
    let mut by_mtime: Vec<(std::time::SystemTime, PathBuf)> = files
        .into_iter()
        .map(|p| {
            let mtime = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, p)
        })
        .collect();
    by_mtime.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    by_mtime.into_iter().find_map(|(_, path)| {
        let head = read_head_bounded(&path, budget)?;
        codex_session_matches_cwd(&head, cwd).then_some(path)
    })
}

/// Whether a codex rollout's `session_meta` line names `cwd`. Only the head of
/// the file is scanned — `session_meta` is written first, and reading further
/// would cost a full parse of every rollout on the host.
fn codex_session_matches_cwd(content: &str, cwd: &str) -> bool {
    for line in content.lines().take(8) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload_cwd = value
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .or_else(|| value.get("cwd"))
            .and_then(Value::as_str);
        return payload_cwd == Some(cwd);
    }
    false
}

/// Parse one already-read transcript, trying the plane the adapter names
/// first.
///
/// Both parsers are total and shape-selective — a codex rollout run through
/// the claude parser yields nothing, and vice versa — so trying both costs a
/// second pass on a mis-recorded adapter and never the answer.
fn parse_by_adapter(content: &str, adapter: Option<&str>) -> Option<ResolvedThread> {
    let claude = || {
        let entries = parse_claude_transcript(content);
        (!entries.is_empty()).then_some(ResolvedThread {
            source: ThreadSource::ClaudeTranscript,
            entries,
        })
    };
    let codex = || {
        let entries = parse_codex_rollout(content);
        (!entries.is_empty()).then_some(ResolvedThread {
            source: ThreadSource::CodexRollout,
            entries,
        })
    };
    if adapter == Some("codex") {
        codex().or_else(claude)
    } else {
        claude().or_else(codex)
    }
}

/// A transcript read, with everything the caller needs to answer *and* to
/// keep the next read cheap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRead {
    /// The thread itself.
    pub thread: ResolvedThread,
    /// The file it came from, so the caller can pin it in the locator.
    pub path: PathBuf,
    /// Whether [`MAX_TRANSCRIPT_BYTES`] cut off the oldest part of the file.
    pub truncated: bool,
}

/// Read the agent transcript for a molecule, using its durable locator.
///
/// Resolution order, and why it is this one:
///
/// 1. **The pinned transcript path**, when the locator carries one. This is
///    the O(1) path: one open, bounded bytes, no directory walk anywhere.
/// 2. **The claude project directory** for the recorded cwd — a directory
///    listing plus one open.
/// 3. **The codex rollout tree** — the only scan left, newest-first and
///    head-only, and it stops being taken as soon as step 1 can answer.
///
/// `adapter` selects which of 2/3 is tried first; both are tried, because a
/// mis-recorded adapter should cost accuracy of ordering, never the whole
/// answer.
///
/// Returns `None` when neither plane resolves — the caller then falls back to
/// the pane.
#[must_use]
pub fn read_transcript_for(
    roots: &AgentSessionRoots,
    locator: &SessionLocator,
    budget: &mut ReadBudget,
) -> Option<TranscriptRead> {
    let adapter = locator.adapter.as_deref();
    if let Some(pinned) = locator.transcript.as_deref() {
        let path = PathBuf::from(pinned);
        if let Some((content, truncated)) = read_tail_bounded(&path, MAX_TRANSCRIPT_BYTES, budget) {
            if let Some(thread) = parse_by_adapter(&content, adapter) {
                return Some(TranscriptRead {
                    thread,
                    path,
                    truncated,
                });
            }
        }
    }
    let cwd = locator.cwd.as_str();
    let claude = |budget: &mut ReadBudget| {
        let dir = roots.claude_projects.join(sanitise_agent_path(cwd));
        let path = most_recent_jsonl(&dir)?;
        let (content, truncated) = read_tail_bounded(&path, MAX_TRANSCRIPT_BYTES, budget)?;
        let entries = parse_claude_transcript(&content);
        (!entries.is_empty()).then_some(TranscriptRead {
            thread: ResolvedThread {
                source: ThreadSource::ClaudeTranscript,
                entries,
            },
            path,
            truncated,
        })
    };
    let codex = |budget: &mut ReadBudget| {
        let path = codex_log_for_cwd(&roots.codex_sessions, cwd, budget)?;
        let (content, truncated) = read_tail_bounded(&path, MAX_TRANSCRIPT_BYTES, budget)?;
        let entries = parse_codex_rollout(&content);
        (!entries.is_empty()).then_some(TranscriptRead {
            thread: ResolvedThread {
                source: ThreadSource::CodexRollout,
                entries,
            },
            path,
            truncated,
        })
    };
    if adapter == Some("codex") {
        codex(budget).or_else(|| claude(budget))
    } else {
        claude(budget).or_else(|| codex(budget))
    }
}

/// Encode a filesystem path the way the claude agent names its per-project
/// session directory (every non-alphanumeric byte becomes `-`).
///
/// Re-exported from [`cosmon_core::session_thread::sanitise_agent_path`] so
/// the encoding has exactly one definition: `cs peek`'s energy probe resolves
/// the same directory, and two copies of a format contract drift.
fn sanitise_agent_path(path: &str) -> String {
    cosmon_core::session_thread::sanitise_agent_path(path)
}

/// Combine what the transcript plane and the pane plane produced into the one
/// thread the route serves.
///
/// Pure, so the source-selection contract is unit-testable without a live
/// agent: a resolved transcript wins; otherwise a captured pane becomes a
/// [`ThreadSource::TmuxScrollback`] snapshot; otherwise
/// [`ThreadSource::None`] with an empty thread — never a silent empty
/// success.
#[must_use]
pub fn assemble_thread(transcript: Option<ResolvedThread>, pane: Option<&str>) -> ResolvedThread {
    if let Some(t) = transcript {
        return t;
    }
    match pane {
        Some(text) => {
            let entries = entries_from_pane(text);
            if entries.is_empty() {
                ResolvedThread {
                    source: ThreadSource::None,
                    entries,
                }
            } else {
                ResolvedThread {
                    source: ThreadSource::TmuxScrollback,
                    entries,
                }
            }
        }
        None => ResolvedThread {
            source: ThreadSource::None,
            entries: Vec::new(),
        },
    }
}

/// Candidate tmux session names for a molecule, in the order they are tried.
///
/// Three, because two conventions are live: `cs tackle` names the session
/// after the molecule (`cosmon-<id>`, the name
/// [`crate::routes::logs_stream`] pins), while a fleet dispatch records a
/// worker-named session on the molecule. Trying the recorded name first and
/// the convention second means the route answers for both without either
/// having to move.
fn session_candidates(
    molecule_id: &str,
    session_name: Option<&str>,
    worker: Option<&str>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: String| {
        // Not `Vec::dedup` — that only collapses *adjacent* repeats, and the
        // repeat here is at the ends: a fleet dispatch records the worker name
        // as the session name, so candidates 1 and 3 are the same string with
        // the convention between them. Spawning tmux twice for one name is a
        // pointless subprocess on every read.
        if !out.contains(&name) {
            out.push(name);
        }
    };
    if let Some(name) = session_name {
        push(name.to_owned());
    }
    push(format!("cosmon-{molecule_id}"));
    if let Some(worker) = worker {
        push(worker.to_owned());
    }
    out
}

/// Capture the first candidate tmux session that answers, or `None`.
///
/// Read-only by construction: `capture-pane -p` is the only tmux verb this
/// module ever spawns. There is deliberately no `send-keys` anywhere on this
/// path — see the module docs and `tests/session_is_read_only.rs`.
fn capture_first_pane(candidates: &[String]) -> Option<String> {
    for name in candidates {
        let output = std::process::Command::new("tmux")
            .args([
                "-L",
                "cosmon",
                "capture-pane",
                "-t",
                name,
                "-p",
                "-S",
                &format!("-{CAPTURE_LINES}"),
            ])
            .output()
            .ok()?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// The molecule's durable session locator, or one reconstructed from the
/// legacy fleet entry.
///
/// # Why the order is this one
///
/// The locator sidecar is written at dispatch inside the molecule directory,
/// which outlives the worker. The fleet entry does not: `cs done` removes the
/// worker from `fleet.json` (and `cs purge` removes it too), so a molecule
/// closed the normal way had **no** recoverable cwd — the transcript was
/// still on disk and this route answered `source: none`. Reading the sidecar
/// first is the fix; reading the fleet entry second is what keeps molecules
/// dispatched before the sidecar existed readable.
///
/// Returns `None` when neither source knows a working directory.
fn resolve_locator(tenant_state_dir: &Path, data: &MoleculeData) -> Option<SessionLocator> {
    use cosmon_state::StateStore as _;
    let store = cosmon_filestore::FileStore::new(tenant_state_dir);
    if let Some(mut locator) = store.load_session_locator(&data.id) {
        // The molecule's recorded adapter wins over a stale sidecar field:
        // the sidecar is written once, and a re-dispatch under another
        // adapter would otherwise keep pointing at the first one's plane.
        if locator.adapter.is_none() {
            locator.adapter.clone_from(&data.adapter);
        }
        return Some(locator);
    }
    let worker_id = data.assigned_worker.as_ref()?;
    let fleet = store.load_fleet().ok()?;
    let repo = fleet.workers.get(worker_id)?.repo.clone()?;
    let cwd = match store.project_root() {
        Some(root) => cosmon_filestore::resolve_repo_path(&repo, &root),
        None => PathBuf::from(repo),
    };
    Some(SessionLocator::new(
        cwd.to_string_lossy(),
        data.adapter.clone(),
    ))
}

/// Persist what this read learned, so the next one is cheap.
///
/// Best-effort: a locator that cannot be written costs the *next* request its
/// O(1) path, never this one its answer. This is also the migration path for
/// molecules dispatched before the sidecar existed — their first successful
/// read writes the locator the fleet entry supplied, and from then on
/// teardown cannot take it away.
fn remember_locator(tenant_state_dir: &Path, data: &MoleculeData, locator: &SessionLocator) {
    let store = cosmon_filestore::FileStore::new(tenant_state_dir);
    if store.load_session_locator(&data.id).as_ref() == Some(locator) {
        return;
    }
    let _ = store.save_session_locator(&data.id, locator);
}

/// The waiting verdict, and which plane's text produced it.
///
/// # Why the pane is consulted even when a transcript resolved
///
/// A permission prompt is drawn on the **screen**. The agent writes its own
/// turns to the transcript, but the harness dialogue that stops it — "Do you
/// want to proceed?" — is not always one of them. Classifying `waiting` from
/// transcript entries alone therefore reported a worker frozen at a live
/// prompt as *not waiting*, which is the one direction this field must never
/// be wrong in. So both planes are classified and the *evidence source* is
/// named on the wire rather than left for the reader to guess.
///
/// The transcript is preferred when it fires, because it is the dated,
/// attributed plane; the pane is what catches what the transcript never saw.
#[must_use]
pub fn waiting_with_pane(
    entries: &[ThreadEntry],
    pane: Option<&str>,
    awaiting_operator: bool,
) -> (cosmon_core::session_thread::WaitingVerdict, &'static str) {
    let from_transcript = waiting_from_entries(entries, WAITING_SCAN_ENTRIES, awaiting_operator);
    if from_transcript.class != cosmon_core::dialogue::DialogueClass::None {
        return (from_transcript, "transcript");
    }
    if let Some(text) = pane {
        let pane_entries = entries_from_pane(text);
        let from_pane =
            waiting_from_entries(&pane_entries, WAITING_SCAN_ENTRIES, awaiting_operator);
        if from_pane.class != cosmon_core::dialogue::DialogueClass::None {
            return (from_pane, "pane");
        }
    }
    let source = if awaiting_operator {
        "control-plane"
    } else {
        "none"
    };
    (from_transcript, source)
}

/// Clamp a caller-supplied window size into `[0, MAX_LIMIT]`.
fn clamp_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
}

/// Read both planes for one molecule: the agent transcript, and the live
/// tmux pane.
///
/// Split out of [`get_session`] because it is the whole of the route's
/// **blocking** work, and it is the part with a policy in it: which planes are
/// read, in what order, and what gets remembered afterwards. The route around
/// it is the five-clause admission pipeline plus a projection.
///
/// Everything here runs on the blocking pool. A multi-megabyte file read and
/// a `tmux capture-pane` fork on the async executor stall every other request
/// served by the same worker thread — the read is bounded now, but bounded is
/// not free.
///
/// # Errors
///
/// `Err(())` only when the blocking task itself could not be joined (a
/// panicking or cancelled pool task). Every *absence* — no locator, no
/// transcript, no pane — is a `None`, because "nothing was retrievable" is an
/// answer this route must give as a 200.
async fn resolve_session_planes(
    tenant_state_dir: &Path,
    data: &MoleculeData,
    molecule_id_str: &str,
) -> Result<(Option<TranscriptRead>, Option<String>), ()> {
    let roots = AgentSessionRoots::from_env();
    let locator = resolve_locator(tenant_state_dir, data);
    let candidates = session_candidates(
        molecule_id_str,
        data.session_name.as_deref(),
        data.assigned_worker
            .as_ref()
            .map(cosmon_core::id::WorkerId::as_str),
    );
    // The pane is captured whenever the molecule could still be at a live
    // prompt — a terminal molecule has no pane, and asking tmux about one
    // would be a fork per request for a guaranteed miss.
    let want_pane = !data.status.is_terminal();
    let (locator, transcript, pane) = tokio::task::spawn_blocking(move || {
        let mut budget = ReadBudget::default();
        let transcript = locator
            .as_ref()
            .and_then(|l| read_transcript_for(&roots, l, &mut budget));
        let pane = if want_pane || transcript.is_none() {
            capture_first_pane(&candidates)
        } else {
            None
        };
        (locator, transcript, pane)
    })
    .await
    .map_err(|_| ())?;

    // Pin what was resolved, so the next read opens one file instead of
    // scanning, and so a legacy molecule stops depending on its fleet entry.
    if let Some(mut locator) = locator {
        if let Some(found) = transcript.as_ref() {
            locator = locator.with_transcript(found.path.to_string_lossy());
        }
        remember_locator(tenant_state_dir, data, &locator);
    }
    Ok((transcript, pane))
}

/// `GET /v1/molecules/{id}/session` — see module docs.
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, ApiError> {
    // 1 & 2 — bearer + JWT.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 3 — the worker-output scope (see module docs for why not a new one).
    authorise_scope_public(&state, &jwt, "session", &[LOGS_SUBSCRIBE], LOGS_SUBSCRIBE)?;

    // 4 — admission boundary: the session is worker output for one molecule.
    let spark = build_spark_public(&state, &jwt, Verb::SubscribeLogs, Some(&molecule_id_str))?;

    // 5 — a malformed id is a 404, never a 400 (no existence oracle), and it
    //     never reaches a tmux argument.
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    let (view, tenant_state_dir) =
        observe_with_state_dir_public(&state, &spark, &jwt, &molecule_id)?;
    let data = &view.data;

    let (transcript, pane) = resolve_session_planes(&tenant_state_dir, data, &molecule_id_str)
        .await
        .map_err(|()| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            label: "internal",
            request_id: Some(spark.request_id.clone()),
        })?;

    let truncated = transcript.as_ref().is_some_and(|t| t.truncated);
    let live = pane.is_some()
        || data
            .process
            .as_ref()
            .is_some_and(cosmon_core::process::MoleculeProcess::is_active);
    let thread = assemble_thread(transcript.map(|t| t.thread), pane.as_deref());

    // The waiting verdict: the pane/text classifier plus the control-plane
    // `await-operator` witnesses. The durable `blocked_on.json` is the belt to
    // the tag's suspenders — a reconcile that drops tags must not silently
    // make a blocked worker look busy.
    let awaiting_operator = cosmon_core::operator_block::awaits_operator(&data.tags)
        || tenant_state_dir
            .join("fleets")
            .join(data.fleet_id.as_str())
            .join("molecules")
            .join(data.id.as_str())
            .join("blocked_on.json")
            .exists();
    let (waiting, waiting_evidence_source) =
        waiting_with_pane(&thread.entries, pane.as_deref(), awaiting_operator);

    // Page: `tail` wins over `offset`/`limit`.
    let total = thread.entries.len();
    let (first_index, shown) = if let Some(n) = query.tail {
        let n = n.min(MAX_LIMIT);
        (total.saturating_sub(n), tail(&thread.entries, n))
    } else {
        let from = query.offset.unwrap_or(0);
        (
            from.min(total),
            window(&thread.entries, from, clamp_limit(query.limit)),
        )
    };

    Ok(Json(json!({
        "request_id": spark.request_id,
        "molecule_id": molecule_id_str,
        "status": data.status.to_string(),
        "adapter": data.adapter,
        "source": thread.source.as_str(),
        "retrospective": thread.source.is_retrospective(),
        "live": live,
        "total": total,
        "offset": first_index,
        "returned": shown.len(),
        "truncated": truncated,
        "waiting": {
            "waiting": waiting.waiting,
            "class": waiting.class.as_str(),
            "evidence": waiting.evidence,
            "evidence_source": waiting_evidence_source,
            "awaiting_operator": waiting.awaiting_operator,
        },
        "entries": shown.iter().map(|e| json!({
            "ordinal": e.ordinal,
            "at": e.at,
            "origin": e.origin.as_str(),
            "text": e.text,
        })).collect::<Vec<_>>(),
    })))
}

/// Extract the JWT bearer from the `Authorization` header. Route-local for the
/// same reason the sibling modules keep their own: the helper is tiny and a
/// shared one would couple the route modules for no gain.
fn extract_bearer(headers: &HeaderMap) -> Result<&str, RppRejectReason> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or(RppRejectReason::MissingAuthorization)?;
    let s = header.to_str().map_err(|_| RppRejectReason::MalformedJwt)?;
    let stripped = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .ok_or(RppRejectReason::MalformedJwt)?;
    Ok(stripped.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_LOG: &str = r##"{"type":"user","timestamp":"2026-09-07T18:50:05.678Z","message":{"role":"user","content":"brief"}}
{"type":"assistant","timestamp":"2026-09-07T18:50:07.475Z","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"##;

    const CODEX_LOG: &str = r##"{"type":"session_meta","timestamp":"2026-09-07T16:34:25.000Z","payload":{"cwd":"/work/tree","session_id":"s"}}
{"type":"response_item","timestamp":"2026-09-07T16:34:36.465Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"codex speaking"}]}}"##;

    fn roots_with(claude: &Path, codex: &Path) -> AgentSessionRoots {
        AgentSessionRoots {
            claude_projects: claude.to_path_buf(),
            codex_sessions: codex.to_path_buf(),
        }
    }

    /// A locator for `cwd` with nothing pinned yet — the state at dispatch.
    fn locator_for(cwd: &str, adapter: Option<&str>) -> SessionLocator {
        SessionLocator::new(cwd, adapter.map(ToOwned::to_owned))
    }

    #[test]
    fn claude_transcript_resolves_from_the_recorded_cwd() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let cwd = Path::new("/work/tree");
        let dir = claude
            .path()
            .join(sanitise_agent_path(&cwd.to_string_lossy()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.jsonl"), CLAUDE_LOG).unwrap();

        let mut budget = ReadBudget::default();
        let t = read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator_for("/work/tree", None),
            &mut budget,
        )
        .expect("the transcript resolves without any live pane");
        assert_eq!(t.thread.source, ThreadSource::ClaudeTranscript);
        assert_eq!(t.thread.entries.len(), 2);
        assert_eq!(t.thread.entries[1].text, "working");
        assert!(!t.truncated);
        assert_eq!(budget.files_opened, 1, "one open for one transcript");
    }

    #[test]
    fn codex_rollout_resolves_by_session_meta_cwd() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let nested = codex.path().join("2026").join("09");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("rollout-x.jsonl"), CODEX_LOG).unwrap();

        let mut budget = ReadBudget::default();
        let t = read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator_for("/work/tree", Some("codex")),
            &mut budget,
        )
        .expect("the rollout resolves by its session_meta cwd");
        assert_eq!(t.thread.source, ThreadSource::CodexRollout);
        assert_eq!(t.thread.entries[0].text, "codex speaking");
    }

    #[test]
    fn a_rollout_for_another_cwd_is_not_this_molecules_thread() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        std::fs::write(codex.path().join("rollout-x.jsonl"), CODEX_LOG).unwrap();
        let mut budget = ReadBudget::default();
        assert!(read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator_for("/some/other/tree", Some("codex")),
            &mut budget,
        )
        .is_none());
    }

    /// The bounded-read falsifier, in the shape the finding names: a host
    /// carrying many large unrelated rollouts, and one `tail`-style read.
    ///
    /// Before the change this opened **and fully read** every file in the
    /// tree; the assertion below is on opens and bytes, not on wall-clock, so
    /// it cannot pass by being run on a fast machine.
    #[test]
    fn a_pinned_transcript_costs_one_open_however_large_the_host_tree_is() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        // Twelve unrelated rollouts of ~200 KiB each: 2.4 MiB nobody asked
        // for.
        let noise = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"/elsewhere\"}}}}\n{}",
            "x".repeat(200_000)
        );
        for i in 0..12 {
            std::fs::write(codex.path().join(format!("rollout-{i}.jsonl")), &noise).unwrap();
        }
        let mine = codex.path().join("rollout-mine.jsonl");
        std::fs::write(&mine, CODEX_LOG).unwrap();

        let locator =
            locator_for("/work/tree", Some("codex")).with_transcript(mine.to_string_lossy());
        let mut budget = ReadBudget::default();
        let t = read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator,
            &mut budget,
        )
        .expect("the pinned transcript resolves");
        assert_eq!(t.thread.entries[0].text, "codex speaking");
        assert_eq!(
            budget.files_opened, 1,
            "a pinned locator must open exactly the pinned file"
        );
        assert!(
            budget.bytes_read < 4_096,
            "read {} bytes for a two-line transcript",
            budget.bytes_read
        );
    }

    /// The unpinned scan is still bounded: candidates are probed by their
    /// head, so an unrelated 200 KiB rollout costs a page, not 200 KiB.
    #[test]
    fn an_unpinned_scan_reads_only_the_head_of_a_candidate() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let noise = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"/elsewhere\"}}}}\n{}",
            "x".repeat(400_000)
        );
        std::fs::write(codex.path().join("rollout-noise.jsonl"), &noise).unwrap();

        let mut budget = ReadBudget::default();
        assert!(read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator_for("/work/tree", Some("codex")),
            &mut budget,
        )
        .is_none());
        assert!(
            budget.bytes_read <= MAX_HEAD_BYTES,
            "the metadata probe read {} bytes of a 400 KiB file",
            budget.bytes_read
        );
    }

    #[test]
    fn a_transcript_past_the_ceiling_is_cut_at_its_head_and_says_so() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let cwd = "/work/big";
        let dir = claude.path().join(sanitise_agent_path(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        // One oversized early line, then the two real ones.
        let filler = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}",
            "y".repeat(MAX_TRANSCRIPT_BYTES as usize)
        );
        std::fs::write(&path, format!("{filler}\n{CLAUDE_LOG}")).unwrap();

        let mut budget = ReadBudget::default();
        let t = read_transcript_for(
            &roots_with(claude.path(), codex.path()),
            &locator_for(cwd, None),
            &mut budget,
        )
        .expect("the tail of an oversized transcript still resolves");
        assert!(t.truncated, "the cut must be declared, not hidden");
        assert!(
            budget.bytes_read <= MAX_TRANSCRIPT_BYTES,
            "read {} bytes past the ceiling",
            budget.bytes_read
        );
        assert_eq!(
            t.thread.entries.last().map(|e| e.text.as_str()),
            Some("working"),
            "reading from the end must keep the NEWEST entries"
        );
    }

    #[test]
    fn a_live_pane_prompt_is_seen_even_when_the_transcript_is_calm() {
        // Falsifier for the third finding: the permission prompt is drawn on
        // the screen and never written to the transcript. A transcript-only
        // verdict called this worker busy.
        let entries = entries_from_pane("Compiling cosmon-core\n12 tests running");
        let (calm, source) = waiting_with_pane(&entries, None, false);
        assert!(!calm.waiting);
        assert_eq!(source, "none");

        let (verdict, source) = waiting_with_pane(
            &entries,
            Some("Bash(cargo test)\nDo you want to proceed?\n 1. Yes\n 2. No"),
            false,
        );
        assert!(verdict.waiting, "a live prompt on the pane is waiting");
        assert_eq!(source, "pane");
        assert!(verdict.evidence.is_some());
    }

    #[test]
    fn the_transcript_keeps_the_verdict_when_it_fires_itself() {
        let entries = entries_from_pane("Do you want to proceed?");
        let (verdict, source) = waiting_with_pane(&entries, Some("idle"), false);
        assert!(verdict.waiting);
        assert_eq!(source, "transcript");
    }

    #[test]
    fn the_control_plane_signal_alone_is_named_as_such() {
        let entries = entries_from_pane("running tests");
        let (verdict, source) = waiting_with_pane(&entries, Some("running tests"), true);
        assert!(verdict.waiting);
        assert!(verdict.awaiting_operator);
        assert_eq!(source, "control-plane");
    }

    #[test]
    fn no_transcript_and_no_pane_is_source_none_not_an_empty_success() {
        let t = assemble_thread(None, None);
        assert_eq!(t.source, ThreadSource::None);
        assert!(t.entries.is_empty());
        assert!(!t.source.is_retrospective());
    }

    #[test]
    fn a_live_pane_becomes_an_ordered_scrollback_snapshot() {
        // Falsifier 1: the pane holds N lines; the thread carries those N in
        // order, and `source` names where they came from.
        let t = assemble_thread(None, Some("one\ntwo\nthree\n"));
        assert_eq!(t.source, ThreadSource::TmuxScrollback);
        assert_eq!(
            t.entries
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two", "three"]
        );
        assert!(
            !t.source.is_retrospective(),
            "a screen capture must not claim to carry history"
        );
    }

    #[test]
    fn a_transcript_outranks_a_live_pane() {
        let transcript = ResolvedThread {
            source: ThreadSource::ClaudeTranscript,
            entries: entries_from_pane("from the transcript"),
        };
        let t = assemble_thread(Some(transcript), Some("from the pane"));
        assert_eq!(t.source, ThreadSource::ClaudeTranscript);
        assert_eq!(t.entries[0].text, "from the transcript");
    }

    #[test]
    fn an_empty_pane_capture_is_none_rather_than_a_scrollback_of_nothing() {
        assert_eq!(
            assemble_thread(None, Some("   \n\n")).source,
            ThreadSource::None
        );
    }

    #[test]
    fn limit_is_clamped_to_the_ceiling() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(999_999)), MAX_LIMIT);
    }

    #[test]
    fn session_candidates_prefer_the_recorded_name_then_the_convention() {
        let names = session_candidates(
            "task-20260907-e376",
            Some("issue-51-follow-up-e376"),
            Some("issue-51-follow-up-e376"),
        );
        assert_eq!(names.len(), 2, "a repeated candidate is tried once");
        assert_eq!(names[0], "issue-51-follow-up-e376");
        assert_eq!(names[1], "cosmon-task-20260907-e376");
    }

    #[test]
    fn codex_session_meta_match_is_exact() {
        assert!(codex_session_matches_cwd(CODEX_LOG, "/work/tree"));
        assert!(!codex_session_matches_cwd(CODEX_LOG, "/work"));
        assert!(!codex_session_matches_cwd(
            "{\"type\":\"turn_context\"}",
            "/work/tree"
        ));
    }
}
