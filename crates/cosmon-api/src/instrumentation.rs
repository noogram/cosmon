// SPDX-License-Identifier: AGPL-3.0-only

//! Engine-call instrumentation — IFBDD émission de faits at the
//! cosmon-api boundary.
//!
//! Records every cosmon-engine invocation made by `cs-api` so that the
//! operator can later answer two empirical questions:
//!
//! 1. Per route, which `(verb, args)` pair is invoked, and how long
//!    does it take? The latency baseline drives the decision of whether
//!    a given verb is better served in-process (as a library call) or
//!    by shelling out to a subprocess.
//! 2. Which `(verb, args_hash)` pairs are invoked from at least two
//!    distinct callers? Those are the promotion candidates: a `pub`
//!    function in `cosmon-core` that both callers can reach without
//!    re-shelling out.
//!
//! The instrumentation is **observation, not enforcement**: it never
//! blocks the hot path, never persists in `state.json` (this is system
//! telemetry, not a domain event), and any IO failure is logged via
//! `tracing` and swallowed.
//!
//! # Sinks
//!
//! Two independent sinks fire on every event, each best-effort:
//!
//! - A structured `tracing::info!` event on the
//!   `cosmon_api::engine_call` target — picked up by whatever subscriber
//!   the binary configured (the production binary uses
//!   `tracing-subscriber` with an `EnvFilter`).
//! - When `AppState::resolve_instrumentation_path` returns `Some`, the
//!   event is also appended as a single JSON line to that file. The
//!   intended use is the empirical mini-rapport in `observations.md`:
//!   point the path at a tempfile, exercise the API, then post-process
//!   the captures.

use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::AppState;

/// How the cosmon engine was reached for this call. Today every route
/// in `cs-api` is either a subprocess shell-out via `crate::run_cs`
/// or a direct read of `.cosmon/state/...` JSON files; no in-process
/// write of cosmon state happens (T3 introduces the first via `tag`
/// promotion).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
    /// The handler shelled out to the `cs` binary as a child process.
    SubprocessShellOut,
    /// The handler read `.cosmon/state/...` JSON files directly without
    /// going through `cs`. The whisper inbox scan and the
    /// `/inbox` / `/ensemble` / `/peek` / `/galaxies` aggregators all
    /// fall here.
    InProcessStateRead,
    /// The handler mutated on-disk state in-process. Reserved for T3
    /// (in-process `tag` write); also used today for whisper-archive
    /// (a filesystem `rename` outside `state.json`).
    InProcessStateWrite,
}

/// One recorded engine-call event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineCallEntered {
    /// The cosmon verb invoked (e.g. `tag`, `tackle`, `observe`,
    /// `--version`). For in-process scans the verb is a synthetic
    /// `<scan-...>` label since there is no real `cs` subcommand on
    /// this path today.
    pub verb: String,
    /// Stable hash of the argument vector. Lets us detect "same
    /// `(verb, args)` invoked from two distinct callers" without
    /// keeping the raw args (which can carry user-supplied content).
    pub args_hash: u64,
    /// HTTP route or in-process callsite that triggered the call.
    pub caller: String,
    /// How the cosmon engine was reached.
    pub mode: InvocationMode,
    /// Wall-clock latency of the call envelope, in milliseconds.
    pub latency_ms: u64,
    /// Number of bytes captured on the child's stdout (subprocess
    /// only). `0` for in-process modes — no stdout produced.
    pub stdout_bytes: u64,
    /// ISO-8601 UTC timestamp of when the event was recorded.
    pub timestamp: String,
}

/// Compute a stable hash of an argument vector. Stable within a single
/// process — sufficient for the analysis, which only compares callers
/// against each other within one log capture window.
pub fn hash_args(args: &[&str]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for a in args {
        a.hash(&mut h);
        // Separator so `["a","bc"]` and `["ab","c"]` do not collide.
        0u8.hash(&mut h);
    }
    h.finish()
}

/// Pick the first argv token that is not a long flag, so the verb of
/// `["--json", "tag", "id", "--add", "x"]` is `"tag"`. Falls back to
/// the raw first token (or `"?"`) when every argument is a flag — that
/// way `["--version"]` still gets a recognisable label.
pub fn first_non_flag_verb(args: &[&str]) -> String {
    for a in args {
        if !a.starts_with("--") {
            return (*a).to_owned();
        }
    }
    args.first().copied().unwrap_or("?").to_owned()
}

/// Current UTC timestamp in ISO-8601 with millisecond precision —
/// matches the format used by `cs` events on disk.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Append the event as a single JSON line to the path resolved from
/// `state`. Best-effort: failure is logged and swallowed.
fn append_ndjson(state: &AppState, event: &EngineCallEntered) {
    let Some(path) = state.resolve_instrumentation_path() else {
        return;
    };
    let line = match serde_json::to_string(event) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                target: "cosmon_api::instrumentation",
                error = %e,
                "serialize engine_call event"
            );
            return;
        }
    };
    let _guard = FILE_LOCK.lock().ok();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = appended {
        tracing::warn!(
            target: "cosmon_api::instrumentation",
            error = %e,
            path = %path.display(),
            "append engine_call event"
        );
    }
}

/// Emit the event on every configured sink. Defensive — must never
/// panic and must never block the caller on an IO error.
pub(crate) fn emit(state: &AppState, event: EngineCallEntered) {
    tracing::info!(
        target: "cosmon_api::engine_call",
        verb = %event.verb,
        args_hash = event.args_hash,
        caller = %event.caller,
        mode = ?event.mode,
        latency_ms = event.latency_ms,
        stdout_bytes = event.stdout_bytes,
        timestamp = %event.timestamp,
        "engine_call_entered"
    );
    append_ndjson(state, &event);
}

/// Wrap an in-process closure with timing + emission. The verb is the
/// synthetic name of the operation (e.g. `<scan-inbox>`) since the
/// handler does not invoke a `cs` subcommand.
pub(crate) fn record_in_process<R>(
    state: &AppState,
    caller: &str,
    verb: &str,
    mode: InvocationMode,
    body: impl FnOnce() -> R,
) -> R {
    let started = Instant::now();
    let result = body();
    let latency_ms = elapsed_ms(started);
    emit(
        state,
        EngineCallEntered {
            verb: verb.to_owned(),
            args_hash: 0,
            caller: caller.to_owned(),
            mode,
            latency_ms,
            stdout_bytes: 0,
            timestamp: now_iso(),
        },
    );
    result
}

/// Convert an `Instant` start tag to a millisecond latency. `as_millis`
/// returns `u128`; we clamp to `u64::MAX` rather than panicking on the
/// ~580 million-year overflow.
pub(crate) fn elapsed_ms(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Read every event from an NDJSON file. Used by the smoke test and by
/// the `observations.md` mini-rapport tooling. Returns an empty `Vec`
/// when the file does not exist.
pub fn read_ndjson(path: &std::path::Path) -> std::io::Result<Vec<EngineCallEntered>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<EngineCallEntered>(trimmed) {
            Ok(ev) => out.push(ev),
            Err(e) => tracing::warn!(
                target: "cosmon_api::instrumentation",
                error = %e,
                "skip malformed ndjson line"
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_args_distinguishes_concatenations() {
        assert_ne!(hash_args(&["a", "bc"]), hash_args(&["ab", "c"]));
    }

    #[test]
    fn hash_args_is_deterministic_within_process() {
        let a = hash_args(&["--json", "tackle", "task-1"]);
        let b = hash_args(&["--json", "tackle", "task-1"]);
        assert_eq!(a, b);
    }

    #[test]
    fn first_non_flag_verb_skips_json_flag() {
        assert_eq!(first_non_flag_verb(&["--json", "tackle", "x"]), "tackle");
        assert_eq!(first_non_flag_verb(&["--version"]), "--version");
        assert_eq!(first_non_flag_verb(&[]), "?");
    }

    #[test]
    fn read_ndjson_returns_empty_when_missing() {
        let path = std::path::PathBuf::from("/tmp/cosmon-api-instr-does-not-exist.ndjson");
        let events = read_ndjson(&path).unwrap();
        assert!(events.is_empty());
    }
}
