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
//! 1. **claude transcript** — `<claude-config>/projects/{sanitised-cwd}/*.jsonl`,
//!    the most recent log for the worker's recorded working directory.
//! 2. **codex rollout** — `~/.codex/sessions/**/rollout-*.jsonl` whose
//!    `session_meta.payload.cwd` is that same directory.
//! 3. **tmux scrollback** — a capture of the live pane, when no transcript
//!    file resolves. Named as such on the wire, with its limits stated.
//! 4. **none** — nothing was retrievable. An explicit `source`, never an
//!    empty `200` that reads like an empty session.
//!
//! Both transcript paths key off the *recorded* worker directory, so they
//! answer long after the pane is gone — which is the whole point of the route.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use cosmon_core::id::MoleculeId;
use cosmon_core::session_thread::{
    entries_from_pane, parse_claude_transcript, parse_codex_rollout, tail, waiting_from_entries,
    window, ThreadEntry, ThreadSource, WAITING_SCAN_ENTRIES,
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

/// The most-recently-modified `*.jsonl` directly inside `dir`.
///
/// Most recent wins because a worktree can host several attempts (a resume, a
/// retry): the log still being appended to is the current attempt's, and an
/// abandoned earlier one is at worst not shown — never mixed in.
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
fn codex_log_for_cwd(root: &Path, cwd: &str) -> Option<(PathBuf, String)> {
    let mut files = Vec::new();
    walk_jsonl(root, &mut files);
    let mut best: Option<(std::time::SystemTime, PathBuf, String)> = None;
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !codex_session_matches_cwd(&content, cwd) {
            continue;
        }
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _, _)| mtime >= *t) {
            best = Some((mtime, path, content));
        }
    }
    best.map(|(_, p, c)| (p, c))
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

/// Read the agent transcript for a worker whose working directory is known.
///
/// `adapter` is the molecule's recorded adapter (`claude`, `codex`, an
/// in-process provider, or `None` for the legacy case where none was
/// recorded). It selects which plane to try **first**; both are tried, because
/// a mis-recorded adapter should cost accuracy of ordering, never the whole
/// answer.
///
/// Returns `None` when neither plane resolves — the caller then falls back to
/// the pane.
#[must_use]
pub fn read_transcript_thread(
    roots: &AgentSessionRoots,
    cwd: &Path,
    adapter: Option<&str>,
) -> Option<ResolvedThread> {
    let claude = || {
        let dir = roots
            .claude_projects
            .join(sanitise_agent_path(&cwd.to_string_lossy()));
        let path = most_recent_jsonl(&dir)?;
        let content = std::fs::read_to_string(path).ok()?;
        let entries = parse_claude_transcript(&content);
        (!entries.is_empty()).then_some(ResolvedThread {
            source: ThreadSource::ClaudeTranscript,
            entries,
        })
    };
    let codex = || {
        let (_, content) = codex_log_for_cwd(&roots.codex_sessions, &cwd.to_string_lossy())?;
        let entries = parse_codex_rollout(&content);
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

/// The working directory `cs tackle` recorded for this molecule's worker.
///
/// Filesystem-derived and pane-independent, which is what makes the transcript
/// readable post-mortem: the pane is gone, the fleet entry is not.
fn recorded_worker_cwd(tenant_state_dir: &Path, data: &MoleculeData) -> Option<PathBuf> {
    use cosmon_state::StateStore as _;
    let worker_id = data.assigned_worker.as_ref()?;
    let store = cosmon_filestore::FileStore::new(tenant_state_dir);
    let fleet = store.load_fleet().ok()?;
    let repo = fleet.workers.get(worker_id)?.repo.clone()?;
    Some(match store.project_root() {
        Some(root) => cosmon_filestore::resolve_repo_path(&repo, &root),
        None => PathBuf::from(repo),
    })
}

/// Clamp a caller-supplied window size into `[0, MAX_LIMIT]`.
fn clamp_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
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

    // Resolve the thread: transcript first, pane as the snapshot fallback.
    let roots = AgentSessionRoots::from_env();
    let transcript = recorded_worker_cwd(&tenant_state_dir, data)
        .and_then(|cwd| read_transcript_thread(&roots, &cwd, data.adapter.as_deref()));
    let pane = if transcript.is_some() {
        None
    } else {
        capture_first_pane(&session_candidates(
            &molecule_id_str,
            data.session_name.as_deref(),
            data.assigned_worker
                .as_ref()
                .map(cosmon_core::id::WorkerId::as_str),
        ))
    };
    let live = pane.is_some()
        || data
            .process
            .as_ref()
            .is_some_and(cosmon_core::process::MoleculeProcess::is_active);
    let thread = assemble_thread(transcript, pane.as_deref());

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
    let waiting = waiting_from_entries(&thread.entries, WAITING_SCAN_ENTRIES, awaiting_operator);

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
        "waiting": {
            "waiting": waiting.waiting,
            "class": waiting.class.as_str(),
            "evidence": waiting.evidence,
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

        let t = read_transcript_thread(&roots_with(claude.path(), codex.path()), cwd, None)
            .expect("the transcript resolves without any live pane");
        assert_eq!(t.source, ThreadSource::ClaudeTranscript);
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[1].text, "working");
    }

    #[test]
    fn codex_rollout_resolves_by_session_meta_cwd() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let nested = codex.path().join("2026").join("09");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("rollout-x.jsonl"), CODEX_LOG).unwrap();

        let t = read_transcript_thread(
            &roots_with(claude.path(), codex.path()),
            Path::new("/work/tree"),
            Some("codex"),
        )
        .expect("the rollout resolves by its session_meta cwd");
        assert_eq!(t.source, ThreadSource::CodexRollout);
        assert_eq!(t.entries[0].text, "codex speaking");
    }

    #[test]
    fn a_rollout_for_another_cwd_is_not_this_molecules_thread() {
        let claude = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        std::fs::write(codex.path().join("rollout-x.jsonl"), CODEX_LOG).unwrap();
        assert!(read_transcript_thread(
            &roots_with(claude.path(), codex.path()),
            Path::new("/some/other/tree"),
            Some("codex"),
        )
        .is_none());
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
