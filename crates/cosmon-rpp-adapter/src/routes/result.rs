// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/result` — canonical-deliverable endpoint.
//!
//! # The gap this closes
//!
//! A tenant tackles a molecule, it completes — and then *no* API path
//! returns its output. `GET .../artifacts` reads the ephemeral
//! `COSMON_ARTIFACT_DIR` (`/tmp/cosmon/...`, tmpfs) which a default
//! `task-work` worker never writes to: the worker deposits its
//! deliverable in its git *worktree*, and no formula contract obliges
//! it to copy that into the artifact dir. So `artifacts` returns `[]`
//! and `GET /v1/molecules/{id}` carries no result. The first molecule
//! of an onboarding tenant (Dave's haiku, 2026-06-05) was
//! unrecoverable.
//!
//! # Why a `result` route rather than only fixing the worker
//!
//! Reading from the *persistent* molecule directory
//! (`<state_dir>/fleets/<fleet>/molecules/<id>/`) is robust and
//! independent of the worker's goodwill: panel formulas already write
//! `synthesis.md` there at every run, so `deep-think` molecules become
//! retrievable with **zero** worker change. The artifact dir
//! (`COSMON_ARTIFACT_DIR`) is a *fallback* for one-shot `task-work`
//! molecules whose formula contract now asks the worker to deposit a
//! `result.md` there. The endpoint unifies both planes behind one
//! resolution order — see [`resolve_canonical_result`].
//!
//! # Resolution order (first match wins)
//!
//! 1. `<molecule_dir>/result.md` — explicit canonical-deliverable
//!    convention (persistent; survives the tmpfs).
//! 2. `<molecule_dir>/synthesis.md` — panel / `deep-think` formulas
//!    (persistent).
//! 3. `<artifact_dir>/result.md` — the `task-work` formula contract
//!    output (ephemeral, but fresh right after tackle).
//! 4. `<artifact_dir>/<single file>` — if the artifact dir holds
//!    exactly one visible file, return it. Disambiguated to a file
//!    named `result.*` when several are present.
//!
//! # The honest `result_status` (C1)
//!
//! The old contract answered `404 result_not_available` whenever
//! resolution found nothing. That one 404 covered **at least six
//! distinct worlds** — not-started, running, ready, finished-without-a-
//! deliverable, stalled, failed — so a tenant polling for output could
//! not tell *"keep waiting"* from *"relaunch, it's dead"*. Dave's
//! haiku stalled forever behind a silent, eternal 404.
//!
//! This endpoint now returns **`200` for every molecule that exists**
//! (an *absent* molecule, or one belonging to another tenant, still
//! collapses to `404 not_found` — the turing no-existence-oracle
//! invariant is untouched). The body carries a derived
//! [`ResultStatus`] field plus a raw `liveness` block. The status is
//! **never stored**: it is a pure function of the loaded molecule
//! state and the wall clock (see [`derive_result_status`]), exactly
//! like [`resolve_canonical_result`] is a pure function of the disk.
//!
//! Two non-negotiable garde-fous (anti-PASS, anti-`aec8`):
//!
//! * **`ready` is proven by the DISK, never by `status == completed`.**
//!   A worker can write `completed` without depositing anything (the
//!   v1.9 fabricated-`latest` bug). `completed` + empty resolution =
//!   `done-no-deliverable`, *not* `ready`. We go further: `ready` is
//!   emitted only once the bytes have actually been *read*, so a file
//!   that vanishes between probe and read degrades honestly.
//! * **One field, not N endpoints.** There is no `/result/stalled`
//!   sibling — that would be the per-state verifier `aec8` forbids.
//!
//! `stalled` is decided **by the `tackled_at` timeout first** (godel:
//! bound the wait by decree rather than try to solve the halting
//! problem) and, as a faster local signal, by a dead worker process.
//! C2 (the watchdog) will later *upgrade* the precision via a real
//! worker heartbeat — a **soft** dependency surfaced today as
//! `liveness.heartbeat_at` (proxied by `last_progress_at`); C1 ships
//! without waiting for it.
//!
//! # Pipeline (mirrors the molecule routes' shape)
//!
//! 1. Extract bearer.
//! 2. Validate JWT.
//! 3. Scope check — `cosmon:molecule:read` (or `:write`, which implies
//!    read). The canonical result *is* the molecule's output; reading
//!    it is part of observing the molecule, so it rides the molecule
//!    scope rather than the independent `:artifact:read` scope. This
//!    keeps the onboarding flow working with the basic read grant.
//! 4. Admission boundary (`http_request_to_spark`, reusing the
//!    `ObserveMolecule` verb — this is a molecule read).
//! 5. Library-direct load of the molecule (tenant-isolated), then a
//!    pure filesystem resolution against the two candidate dirs.
//! 6. Derive `result_status`, attach `liveness`, and return the
//!    deliverable inline (UTF-8 text) or base64-encoded (binary) when
//!    one resolved — or `result: null` when none did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::MoleculeData;
use serde_json::{json, Value};

use crate::admission::Verb;
use crate::auth::scopes::{MOLECULE_READ, MOLECULE_WRITE};
use crate::error::{ApiError, RppRejectReason};
use crate::jwt::JwtVerifier;
use crate::routes::artifacts::{artifact_dir_for, detect_content_type};
use crate::routes::molecules::{
    authorise_scope_public, build_spark_public, observe_with_state_dir_public,
};
use crate::AppState;

/// A canonical deliverable resolved on disk: the human-facing source
/// label (`result.md`, `synthesis.md`, `artifact:<name>`) and the
/// absolute path it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResult {
    /// Stable source label echoed on the wire so the tenant knows which
    /// convention produced the bytes.
    pub source: String,
    /// Absolute path the bytes are read from.
    pub path: PathBuf,
}

/// Pure resolution of a molecule's canonical deliverable across the
/// persistent molecule dir and the ephemeral artifact dir.
///
/// Both directories may be absent — a freshly nucleated molecule has
/// neither. The function performs no reads of file *contents*; it only
/// probes existence and (for the single-file artifact fallback)
/// enumerates the artifact dir. Returning the path rather than the
/// bytes keeps the function trivially testable.
///
/// See the module docs for the resolution order.
#[must_use]
pub fn resolve_canonical_result(
    molecule_dir: &Path,
    artifact_dir: &Path,
) -> Option<ResolvedResult> {
    // 1 & 2 — persistent molecule dir, explicit then panel convention.
    for name in ["result.md", "synthesis.md"] {
        let candidate = molecule_dir.join(name);
        if candidate.is_file() {
            return Some(ResolvedResult {
                source: name.to_owned(),
                path: candidate,
            });
        }
    }

    // 3 — formula-contract output in the artifact dir.
    let art_result = artifact_dir.join("result.md");
    if art_result.is_file() {
        return Some(ResolvedResult {
            source: "artifact:result.md".to_owned(),
            path: art_result,
        });
    }

    // 4 — single-file artifact fallback. Collect visible regular files;
    //     a lone file is unambiguous, several disambiguate to `result.*`.
    let files = visible_files(artifact_dir);
    match files.as_slice() {
        [single] => Some(ResolvedResult {
            source: format!("artifact:{}", file_name_lossy(single)),
            path: single.clone(),
        }),
        [] => None,
        many => many
            .iter()
            .find(|p| {
                file_name_lossy(p)
                    .rsplit_once('.')
                    .is_some_and(|(stem, _)| stem.eq_ignore_ascii_case("result"))
            })
            .map(|p| ResolvedResult {
                source: format!("artifact:{}", file_name_lossy(p)),
                path: p.clone(),
            }),
    }
}

/// List visible (non-dotfile) regular files directly under `dir`,
/// sorted by name for determinism. Missing dir → empty.
fn visible_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| !file_name_lossy(p).starts_with('.'))
        .collect();
    out.sort();
    out
}

/// Basename of a path as a `String`, lossily. Empty when the path has
/// no final component (never the case for the files we enumerate).
fn file_name_lossy(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Derived availability/liveness of a molecule's canonical result.
///
/// **Never persisted.** A pure projection of the molecule state and the
/// wall clock — see [`derive_result_status`]. The six variants form the
/// minimal-and-complete taxonomy (godel §3): every wire value tells the
/// client one of two things — *keep waiting* (`pending` / `running`),
/// *read it* (`ready` / `done-no-deliverable`), or *relaunch*
/// (`stalled` / `failed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultStatus {
    /// Nucleated but never tackled — no worker has touched it yet.
    Pending,
    /// A worker is bound, the process is up, and the tackle is recent —
    /// poll again.
    Running,
    /// A canonical deliverable was resolved *and read* from disk. The
    /// response body carries it. Proven by bytes, never by `status`.
    Ready,
    /// Lifecycle says `completed` but disk resolution is empty — the
    /// worker finished its formula without leaving a single canonical
    /// deliverable. Read `GET .../artifacts` instead. **Not** `ready`.
    DoneNoDeliverable,
    /// `running` past the `tackled_at` decree, or its worker process is
    /// no longer active. The wait is bounded; relaunch.
    Stalled,
    /// Collapsed, frozen, or starved — the run is broken or arrested and
    /// needs operator action.
    Failed,
}

impl ResultStatus {
    /// Stable kebab-case wire tag. This is the value C4 (actionable
    /// client error) consumes off `body["result_status"]`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ready => "ready",
            Self::DoneNoDeliverable => "done-no-deliverable",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
        }
    }
}

/// Default decree past which a `running` molecule with no fresh signal
/// is presumed `stalled`. Fifteen minutes: long enough for a slow LLM
/// step, short enough that a dead worker is not mistaken for a live one
/// for an unbounded time. Overridable per deployment without a recompile
/// via `COSMON_RESULT_STALL_TIMEOUT_S` (see [`stall_timeout`]).
pub const DEFAULT_STALL_TIMEOUT_SECS: i64 = 15 * 60;

/// The stall decree in effect — `DEFAULT_STALL_TIMEOUT_SECS`, or the
/// positive integer in `COSMON_RESULT_STALL_TIMEOUT_S` when set. A
/// malformed or non-positive override falls back to the default rather
/// than producing a zero/negative window that would flag every live
/// worker as stalled.
#[must_use]
pub fn stall_timeout() -> Duration {
    let secs = std::env::var("COSMON_RESULT_STALL_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_STALL_TIMEOUT_SECS);
    Duration::seconds(secs)
}

/// Pure derivation of the [`ResultStatus`] from the loaded molecule
/// state, the on-disk resolution, and `now`.
///
/// `resolved` is `Some` only when a canonical deliverable was *read*
/// from disk (not merely probed) — that is the `ready` proof. The
/// function performs no I/O and takes the clock as a parameter so the
/// full state space is unit-testable without spawning a worker.
///
/// Decision order (first match wins):
///
/// 1. **Hard failure** (`collapsed` / `frozen` / `starved`) → `failed`.
///    Takes precedence over a lingering deliverable: a broken run must
///    not be advertised as finished. The body still carries the bytes
///    when one resolved, so nothing salvageable is hidden.
/// 2. **Disk-proven** (`resolved.is_some()`) → `ready`. Checked before
///    the completed/running split so a deliverable written mid-run is
///    surfaced honestly — and so `completed` alone can *never* mint
///    `ready`.
/// 3. `completed` with empty resolution → `done-no-deliverable`.
/// 4. `pending` / `queued` → `pending`.
/// 5. `running` → `running` iff the worker process is active **and** the
///    tackle is within the [`stall_timeout`] decree; otherwise
///    `stalled`. A missing `tackled_at` is treated as out-of-decree
///    (conservative: an un-stamped runner is assumed stalled).
#[must_use]
pub fn derive_result_status(
    data: &MoleculeData,
    resolved: Option<&ResolvedResult>,
    now: DateTime<Utc>,
    stale_after: Duration,
) -> ResultStatus {
    use MoleculeStatus as S;

    // 1 — a broken/arrested run is `failed` regardless of disk state.
    if matches!(data.status, S::Collapsed | S::Frozen | S::Starved) {
        return ResultStatus::Failed;
    }

    // 2 — `ready` is proven only by a deliverable read from disk.
    if resolved.is_some() {
        return ResultStatus::Ready;
    }

    match data.status {
        // 3 — completed, but resolution was empty.
        S::Completed => ResultStatus::DoneNoDeliverable,
        // 4 — running: live process + within decree, else stalled.
        S::Running => {
            let process_active = data
                .process
                .as_ref()
                .is_some_and(cosmon_core::process::MoleculeProcess::is_active);
            let within_decree = data
                .tackled_at
                .is_some_and(|t| now.signed_duration_since(t) <= stale_after);
            if process_active && within_decree {
                ResultStatus::Running
            } else {
                ResultStatus::Stalled
            }
        }
        // 5 — `pending` / `queued` → `pending`. The `Collapsed` /
        // `Frozen` / `Starved` failure statuses already returned in step
        // 1; a future `#[non_exhaustive]` status we don't yet understand
        // also falls here, the safest "keep observing" default — never a
        // spurious "relaunch".
        _ => ResultStatus::Pending,
    }
}

/// Build the raw `liveness` block (godel F4 — refus d'opacité). The
/// client re-decides if the server's `result_status` policy does not
/// suit it.
///
/// `heartbeat_at` is the freshest liveness chalk-mark cosmon records
/// today (`last_progress_at`, written by `cs patrol`/`cs peek`). C2 will
/// upgrade it to a worker-emitted heartbeat — a soft dependency; the
/// field name is stable now so C4 need not change when C2 lands.
fn liveness_block(data: &MoleculeData, stale_after: Duration) -> Value {
    json!({
        "process": serde_json::to_value(&data.process).unwrap_or(Value::Null),
        "heartbeat_at": data.last_progress_at,
        "tackled_at": data.tackled_at,
        "stale_after_s": stale_after.num_seconds(),
    })
}

/// `GET /v1/molecules/{id}/result` — return the molecule's canonical
/// deliverable. See module docs for the pipeline and resolution order.
pub async fn get_result(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(molecule_id_str): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    // 1 & 2 — bearer + JWT.
    let token = extract_bearer(&headers).map_err(|e| state.reject(e))?;
    let jwt = JwtVerifier::validate(&state.jwks.load(), token, state.posture)
        .map_err(|e| state.reject(e))?;

    // 3 — molecule:read scope (write implies read).
    authorise_scope_public(
        &state,
        &jwt,
        "result",
        &[MOLECULE_READ, MOLECULE_WRITE],
        MOLECULE_READ,
    )?;

    // 4 — admission. The result is a molecule read; reuse the verb.
    let spark = build_spark_public(&state, &jwt, Verb::ObserveMolecule, Some(&molecule_id_str))?;

    // A malformed id is a 404, never a 400 (turing §8.2.3 — no
    // existence oracle on the wire).
    let molecule_id = MoleculeId::new(&molecule_id_str).map_err(|_| ApiError {
        status: StatusCode::NOT_FOUND,
        label: "not_found",
        request_id: Some(spark.request_id.clone()),
    })?;

    // 5 — load the molecule (tenant-isolated) to learn its fleet (for
    //     the persistent dir path) and status (echoed on the wire).
    let (view, tenant_state_dir) =
        observe_with_state_dir_public(&state, &spark, &jwt, &molecule_id)?;

    let molecule_dir = tenant_state_dir
        .join("fleets")
        .join(view.data.fleet_id.as_str())
        .join("molecules")
        .join(view.data.id.as_str());
    let artifact_dir = artifact_dir_for(
        state.artifact_root.as_path(),
        spark.noyau.as_str(),
        view.data.id.as_str(),
    );

    // 6 — resolve, then *read*. `ready` is proven by bytes actually
    //     read, not merely by an `is_file` probe: a deliverable that
    //     vanishes between probe and read degrades honestly to "no
    //     deliverable" rather than minting a false `ready`.
    let result_block =
        resolve_canonical_result(&molecule_dir, &artifact_dir).and_then(|resolved| {
            std::fs::read(&resolved.path)
                .ok()
                .map(|bytes| (resolved, bytes))
        });

    // The molecule EXISTS (observe succeeded), so we always answer 200
    // with a derived status — never the old silent 404. (An *absent*
    // molecule already 404'd inside `observe_with_state_dir_public`,
    // preserving the turing no-existence-oracle invariant.)
    let stale_after = stall_timeout();
    let result_status = derive_result_status(
        &view.data,
        result_block.as_ref().map(|(r, _)| r),
        Utc::now(),
        stale_after,
    );

    let result_json = match result_block {
        Some((resolved, bytes)) => {
            let size_bytes = bytes.len() as u64;
            let hex = blake3::hash(&bytes).to_hex().to_string();
            let name = file_name_lossy(&resolved.path);
            let content_type = detect_content_type(&name).to_owned();

            // UTF-8 deliverables (haiku, synthesis, markdown) inline as
            // text; binary deliverables base64-encode so the JSON
            // envelope stays valid. The tenant reads `encoding` to know
            // which.
            let (encoding, content) = match String::from_utf8(bytes) {
                Ok(text) => ("utf8", text),
                Err(e) => (
                    "base64",
                    base64::engine::general_purpose::STANDARD.encode(e.as_bytes()),
                ),
            };
            json!({
                "source": resolved.source,
                "content_type": content_type,
                "encoding": encoding,
                "content": content,
                "size_bytes": size_bytes,
                "integrity": { "algo": "blake3", "hex": hex },
            })
        }
        None => Value::Null,
    };

    Ok(Json(json!({
        "request_id": spark.request_id,
        "molecule_id": molecule_id_str,
        "status": view.data.status.to_string(),
        "result_status": result_status.as_str(),
        "liveness": liveness_block(&view.data, stale_after),
        "result": result_json,
    })))
}

/// Extract the JWT bearer from the `Authorization` header. Duplicated
/// from the sibling route modules to keep them independent (the helper
/// is tiny; a shared one would need a lint suppression that is more
/// friction than the dup).
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

    #[test]
    fn resolve_prefers_molecule_result_md() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(mol.path().join("result.md"), b"canonical").unwrap();
        std::fs::write(mol.path().join("synthesis.md"), b"panel").unwrap();
        let r = resolve_canonical_result(mol.path(), art.path()).unwrap();
        assert_eq!(r.source, "result.md");
        assert_eq!(r.path, mol.path().join("result.md"));
    }

    #[test]
    fn resolve_falls_back_to_synthesis_md() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(mol.path().join("synthesis.md"), b"panel").unwrap();
        let r = resolve_canonical_result(mol.path(), art.path()).unwrap();
        assert_eq!(r.source, "synthesis.md");
    }

    #[test]
    fn resolve_reads_artifact_result_md_when_molecule_dir_bare() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(art.path().join("result.md"), b"haiku").unwrap();
        let r = resolve_canonical_result(mol.path(), art.path()).unwrap();
        assert_eq!(r.source, "artifact:result.md");
    }

    #[test]
    fn resolve_single_artifact_file_is_unambiguous() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(art.path().join("haiku.txt"), b"qcd").unwrap();
        std::fs::write(art.path().join(".hidden"), b"skip").unwrap();
        let r = resolve_canonical_result(mol.path(), art.path()).unwrap();
        assert_eq!(r.source, "artifact:haiku.txt");
    }

    #[test]
    fn resolve_multiple_artifacts_disambiguate_to_result_prefix() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(art.path().join("notes.txt"), b"a").unwrap();
        std::fs::write(art.path().join("result.json"), b"{}").unwrap();
        let r = resolve_canonical_result(mol.path(), art.path()).unwrap();
        assert_eq!(r.source, "artifact:result.json");
    }

    #[test]
    fn resolve_multiple_ambiguous_artifacts_yields_none() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        std::fs::write(art.path().join("a.txt"), b"a").unwrap();
        std::fs::write(art.path().join("b.txt"), b"b").unwrap();
        assert!(resolve_canonical_result(mol.path(), art.path()).is_none());
    }

    #[test]
    fn resolve_nothing_present_yields_none() {
        let mol = tempfile::tempdir().unwrap();
        let art = tempfile::tempdir().unwrap();
        assert!(resolve_canonical_result(mol.path(), art.path()).is_none());
    }

    #[test]
    fn resolve_tolerates_missing_dirs() {
        let r =
            resolve_canonical_result(Path::new("/nonexistent/mol"), Path::new("/nonexistent/art"));
        assert!(r.is_none());
    }

    // ── derive_result_status — the pure liveness projection ──────────

    /// Build a decodable [`MoleculeData`] from a base envelope merged
    /// with `overrides` (same shape the oidc-testkit plants on disk),
    /// so the derivation can be driven across the whole state space
    /// without spawning a worker.
    fn mol(overrides: Value) -> MoleculeData {
        let mut body = serde_json::json!({
            "id": "task-20260614-x",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": "pending",
            "variables": {},
            "assigned_worker": null,
            "created_at": "2026-06-14T00:00:00Z",
            "updated_at": "2026-06-14T00:00:00Z",
            "total_steps": 1,
            "current_step": 0,
            "completed_steps": [],
            "collapse_reason": null,
            "collapsed_step": null,
            "links": [],
        });
        if let (Value::Object(map), Value::Object(over)) = (&mut body, overrides) {
            for (k, v) in over {
                map.insert(k, v);
            }
        }
        serde_json::from_value(body).expect("decodable MoleculeData")
    }

    /// An active worker process, tackled at `secs_ago`.
    fn active_process() -> Value {
        json!({
            "worker_id": "w-1",
            "tmux_session": "sess-1",
            "started_at": "2026-06-14T00:00:00Z",
            "status": "active",
        })
    }

    fn now() -> DateTime<Utc> {
        "2026-06-14T01:00:00Z".parse().unwrap()
    }

    fn timeout() -> Duration {
        Duration::seconds(DEFAULT_STALL_TIMEOUT_SECS)
    }

    fn ready_marker() -> ResolvedResult {
        ResolvedResult {
            source: "result.md".to_owned(),
            path: PathBuf::from("/seed/result.md"),
        }
    }

    #[test]
    fn derive_pending_when_never_tackled() {
        let m = mol(json!({ "status": "pending" }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::Pending
        );
    }

    #[test]
    fn derive_running_when_fresh_and_process_active() {
        // Tackled 5 min ago (< 15 min decree), process up, no file yet.
        let m = mol(json!({
            "status": "running",
            "tackled_at": "2026-06-14T00:55:00Z",
            "process": active_process(),
        }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::Running
        );
    }

    #[test]
    fn derive_stalled_when_past_tackle_decree() {
        // The kill-the-worker shape: still `running`, process record
        // present, but tackled 1 h ago with nothing on disk.
        let m = mol(json!({
            "status": "running",
            "tackled_at": "2026-06-14T00:00:00Z",
            "process": active_process(),
        }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::Stalled
        );
    }

    #[test]
    fn derive_stalled_when_process_dead_even_if_fresh() {
        // Tackled 1 min ago (within decree) but the worker process is
        // stopped — a dead process beats the clock.
        let m = mol(json!({
            "status": "running",
            "tackled_at": "2026-06-14T00:59:00Z",
            "process": {
                "worker_id": "w-1",
                "tmux_session": "sess-1",
                "started_at": "2026-06-14T00:00:00Z",
                "status": "stopped",
            },
        }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::Stalled
        );
    }

    #[test]
    fn derive_done_no_deliverable_when_completed_without_file() {
        // GARDE-FOU: completed + empty resolution is NEVER ready.
        let m = mol(json!({ "status": "completed" }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::DoneNoDeliverable
        );
    }

    #[test]
    fn derive_ready_only_with_disk_proof() {
        // completed + a file read from disk → ready (proven by bytes).
        let m = mol(json!({ "status": "completed" }));
        assert_eq!(
            derive_result_status(&m, Some(&ready_marker()), now(), timeout()),
            ResultStatus::Ready
        );
    }

    #[test]
    fn derive_ready_when_file_present_mid_run() {
        // A deliverable written while still `running` is honestly ready.
        let m = mol(json!({
            "status": "running",
            "tackled_at": "2026-06-14T00:59:00Z",
            "process": active_process(),
        }));
        assert_eq!(
            derive_result_status(&m, Some(&ready_marker()), now(), timeout()),
            ResultStatus::Ready
        );
    }

    #[test]
    fn derive_failed_when_collapsed() {
        let m = mol(json!({ "status": "collapsed" }));
        assert_eq!(
            derive_result_status(&m, None, now(), timeout()),
            ResultStatus::Failed
        );
    }

    #[test]
    fn derive_failed_beats_disk_for_collapsed() {
        // A broken run is not advertised as ready even if a partial
        // deliverable lingers — the body still carries it, the status is
        // honest about the run.
        let m = mol(json!({ "status": "collapsed" }));
        assert_eq!(
            derive_result_status(&m, Some(&ready_marker()), now(), timeout()),
            ResultStatus::Failed
        );
    }

    #[test]
    fn derive_failed_when_frozen_or_starved() {
        for status in ["frozen", "starved"] {
            let m = mol(json!({ "status": status }));
            assert_eq!(
                derive_result_status(&m, None, now(), timeout()),
                ResultStatus::Failed,
                "status {status} should map to failed"
            );
        }
    }

    #[test]
    fn stall_timeout_default_is_fifteen_minutes() {
        // No env override → the named decree, never a zero window.
        assert_eq!(DEFAULT_STALL_TIMEOUT_SECS, 15 * 60);
    }
}
