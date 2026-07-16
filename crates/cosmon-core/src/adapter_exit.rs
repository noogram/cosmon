// SPDX-License-Identifier: AGPL-3.0-only

//! Structured adapter exit-code contract — cosmon primitive #5.
//!
//! # The bug this primitive makes diagnosable
//!
//! An experiment observed that `codex exec` returns exit `1` *indistinctly*
//! for two structurally different failures: a malformed prompt (the
//! operator's brief is wrong — retrying is pointless) and an over-quota
//! refusal (transient — the same brief will succeed after a backoff). A
//! supervisory recovery loop (`cs patrol`) that sees only "exit 1" cannot
//! tell these apart, so it can neither escalate the first to the operator
//! nor back-off-and-retry the second. Every recoverable stall looks
//! identical to every unrecoverable one.
//!
//! # The contract
//!
//! Cosmon defines **five** canonical exit classes — the complete alphabet
//! a worker-spawn [`crate::spawn_seam`] Adapter may speak when it reports
//! how a worker process ended:
//!
//! | Code | Class | Recoverable? | Meaning |
//! |------|-------|--------------|---------|
//! | `0`  | [`AdapterExitClass::Ok`] | — | clean completion |
//! | `1`  | [`AdapterExitClass::UserError`] | no | the brief/prompt is wrong; retrying re-runs the same mistake |
//! | `64` | [`AdapterExitClass::CredentialError`] | no (needs operator) | missing/invalid credentials — the operator must fix auth |
//! | `65` | [`AdapterExitClass::QuotaError`] | yes (backoff) | rate-limited / over quota — the same work succeeds later |
//! | `66` | [`AdapterExitClass::SpawnError`] | yes (re-spawn) | the worker process never ran (exec failure, killed, OOM) |
//!
//! The numeric values reuse the `sysexits.h` band (`64`–`78`) so that an
//! Adapter realised as a shell wrapper can `exit 64` / `exit 65` / `exit 66`
//! with codes the shell will not clobber — but the *meanings* are cosmon's,
//! not BSD's. This is a deliberate, documented re-use, pinned by
//! [`AdapterExitClass::code`] round-trip tests.
//!
//! # Why this is a Port obligation, not a sibling concern
//!
//! [ADR-079](../../../docs/adr/079-worker-spawn-port-and-adapter-contract.md)
//! §5 lists four obligations every Adapter of the worker-spawn Port must
//! honour (reads `briefing.md`, writable `MOLECULE_DIR`, `cs` on PATH,
//! idempotent termination). This primitive adds a **fifth**: *every Adapter
//! must classify its worker's termination into an [`AdapterExitClass`]*.
//! Without it the supervisory loop is blind. With it `cs patrol` reads one
//! typed verdict and picks the right recovery — [`RecoveryAction`].
//!
//! The classification is **per-Adapter** because the raw signal is
//! per-Adapter: `codex` overloads exit `1`, a future headless API Adapter
//! might surface a `429` in JSON, a shell wrapper might already emit the
//! canonical code directly. [`classify_exit`] dispatches on the Adapter
//! name so each substrate owns its own mapping while the *output alphabet*
//! stays fixed at five classes for every consumer downstream.
//!
//! # Zero I/O
//!
//! Everything here is a pure function of `(adapter_name, raw_exit_code,
//! stderr_tail)`. No process is spawned, no file is read. The caller
//! (`cs patrol`, the spawn-site supervisor) captures the raw triple and
//! asks this module for the verdict.

use serde::{Deserialize, Serialize};

use crate::patrol::PatrolAction;

/// The canonical exit class an Adapter reports for a terminated worker.
///
/// This is the **output alphabet** of the per-Adapter classification: a
/// fixed, total set of five verdicts that every consumer (`cs patrol`,
/// the spawn-site supervisor, the event log) can match exhaustively. The
/// raw process exit code is Adapter-private; this enum is the shared
/// contract.
///
/// Deliberately **not** `#[non_exhaustive]`: the five-class alphabet is the
/// load-bearing contract of this primitive. Widening it is an ADR-grade
/// decision (a new recovery semantics for `cs patrol`), not a silent
/// addition — so downstream exhaustive matches *should* break and force the
/// review when a sixth class is ever proposed. Contrast
/// [`crate::spawn_seam::LoopOwnership`], which *is* `#[non_exhaustive]`
/// because new loop owners are expected to land without changing recovery
/// semantics.
///
/// Wire format is the lowercased variant name (`ok`, `user_error`,
/// `credential_error`, `quota_error`, `spawn_error`) so `events.jsonl`
/// rows are self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterExitClass {
    /// Clean completion. The worker finished its molecule without error.
    /// Canonical code `0`.
    Ok,
    /// The brief, prompt, or operator input was malformed. **Not
    /// recoverable by retry** — re-running replays the same mistake. The
    /// supervisory loop must escalate to the operator. Canonical code `1`.
    UserError,
    /// Missing or invalid credentials (expired API key, no OAuth token,
    /// `401`/`403`). **Not recoverable by cosmon** — the operator must
    /// refresh auth. Canonical code `64`.
    CredentialError,
    /// Rate-limited / over quota / `429` / usage cap. **Recoverable** — the
    /// identical work succeeds after a backoff. This is the class the
    /// motivating experiment showed `codex` hiding inside exit `1`.
    /// Canonical code `65`.
    QuotaError,
    /// The worker process never ran or died before completing (exec
    /// failure, signal kill, OOM, missing binary). **Recoverable** — a
    /// fresh spawn may succeed. Canonical code `66`.
    SpawnError,
}

impl AdapterExitClass {
    /// The canonical numeric exit code for this class.
    ///
    /// An Adapter realised as a shell wrapper emits exactly this integer;
    /// [`from_code`](Self::from_code) is the inverse. The pairing is pinned
    /// by round-trip tests so the wire contract cannot drift.
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::UserError => 1,
            Self::CredentialError => 64,
            Self::QuotaError => 65,
            Self::SpawnError => 66,
        }
    }

    /// Recover an [`AdapterExitClass`] from a canonical numeric code.
    ///
    /// Returns `None` for any integer outside the five-code contract — the
    /// caller then knows the Adapter emitted an *unmapped* raw code and
    /// must run [`classify_exit`] (which inspects stderr) rather than trust
    /// the integer blindly.
    #[must_use]
    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Ok),
            1 => Some(Self::UserError),
            64 => Some(Self::CredentialError),
            65 => Some(Self::QuotaError),
            66 => Some(Self::SpawnError),
            _ => None,
        }
    }

    /// Whether re-running the *same* work could succeed.
    ///
    /// `true` for [`QuotaError`](Self::QuotaError) (backoff) and
    /// [`SpawnError`](Self::SpawnError) (re-spawn). `false` for
    /// [`UserError`](Self::UserError) and [`CredentialError`](Self::CredentialError):
    /// the input or the environment must change first. [`Ok`](Self::Ok) is
    /// `false` because there is nothing to retry.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::QuotaError | Self::SpawnError)
    }

    /// The recovery move `cs patrol` should take for this class.
    ///
    /// This is the whole point of the primitive: a typed verdict in, a
    /// typed corrective action out — no string-matching on stderr at the
    /// patrol layer, because [`classify_exit`] already did it once at the
    /// Adapter boundary.
    #[must_use]
    pub fn recovery(self) -> RecoveryAction {
        match self {
            Self::Ok => RecoveryAction::None,
            Self::UserError => RecoveryAction::EscalateToOperator,
            Self::CredentialError => RecoveryAction::FixCredentials,
            Self::QuotaError => RecoveryAction::BackoffAndRetry,
            Self::SpawnError => RecoveryAction::Respawn,
        }
    }

    /// Project this class onto a transport-layer [`PatrolAction`] for a
    /// specific worker.
    ///
    /// Lets `cs patrol` fold an exit verdict directly into the
    /// [`crate::patrol::PatrolReport`] recommendation list. Retryable
    /// classes become a `RestartWorker`; non-retryable classes become an
    /// `AlertHuman` carrying the reason; [`Ok`](Self::Ok) becomes
    /// `NoAction`.
    #[must_use]
    pub fn to_patrol_action(self, worker_id: crate::id::WorkerId) -> PatrolAction {
        match self.recovery() {
            RecoveryAction::None => PatrolAction::NoAction,
            RecoveryAction::Respawn | RecoveryAction::BackoffAndRetry => {
                PatrolAction::RestartWorker {
                    worker_id,
                    reason: format!("{} ({})", self.summary(), self.recovery().as_str()),
                }
            }
            RecoveryAction::EscalateToOperator | RecoveryAction::FixCredentials => {
                PatrolAction::AlertHuman {
                    message: format!(
                        "worker {worker_id} exited with {}: {} — {}",
                        self.code(),
                        self.summary(),
                        self.recovery().as_str()
                    ),
                }
            }
        }
    }

    /// A short human-readable summary for logs and operator alerts.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Ok => "clean completion",
            Self::UserError => "malformed brief/prompt",
            Self::CredentialError => "invalid or missing credentials",
            Self::QuotaError => "rate-limited / over quota",
            Self::SpawnError => "worker failed to spawn",
        }
    }
}

/// What a supervisory loop (`cs patrol`) should do in response to an
/// [`AdapterExitClass`].
///
/// Kept distinct from [`PatrolAction`] (which is worker-scoped and
/// transport-shaped) so the *policy* (this enum) is decoupled from the
/// *mechanism* (restart this worker / alert this human). A future runtime
/// policy can read [`RecoveryAction`] without depending on the transport
/// patrol's action shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Nothing to do — the worker completed cleanly.
    None,
    /// Wait a backoff interval, then re-run the identical work
    /// ([`AdapterExitClass::QuotaError`]).
    BackoffAndRetry,
    /// Spawn a fresh worker immediately ([`AdapterExitClass::SpawnError`]).
    Respawn,
    /// Surface to the operator — the brief must change before any retry
    /// ([`AdapterExitClass::UserError`]).
    EscalateToOperator,
    /// Surface to the operator — credentials must be refreshed before any
    /// retry ([`AdapterExitClass::CredentialError`]).
    FixCredentials,
}

impl RecoveryAction {
    /// A stable lowercase tag for logs and the event stream.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "no-action",
            Self::BackoffAndRetry => "backoff-and-retry",
            Self::Respawn => "respawn",
            Self::EscalateToOperator => "escalate-to-operator",
            Self::FixCredentials => "fix-credentials",
        }
    }
}

/// Classify a terminated worker into an [`AdapterExitClass`] for a given
/// Adapter.
///
/// This is the per-Adapter mapping the contract requires. The inputs are
/// the raw triple every spawn-site can capture:
///
/// - `adapter` — the validated Adapter name (`codex`, `claude`, …). Selects
///   the substrate-specific disambiguation rules.
/// - `raw_code` — the process exit code, or `None` when the process was
///   killed by a signal (no code).
/// - `stderr_tail` — recent stderr, used to disambiguate codes an Adapter
///   overloads (the `codex` exit-`1` case).
///
/// # Mapping rules (in order)
///
/// 1. `None` (signal kill / never ran) → [`SpawnError`](AdapterExitClass::SpawnError).
/// 2. `Some(0)` → [`Ok`](AdapterExitClass::Ok).
/// 3. `Some(64|65|66)` → trusted directly: an Adapter that *already* speaks
///    the contract (a shell wrapper, a future structured Adapter) is
///    believed without stderr inspection. This is what lets an Adapter
///    "do the mapping itself" and short-circuit the heuristic.
/// 4. Otherwise (the ambiguous band — `Some(1)` and any other non-zero
///    code) → scan `stderr_tail` for quota then credential markers; on a
///    hit return the matching class, else fall back to
///    [`UserError`](AdapterExitClass::UserError) (the conservative,
///    non-retryable default — never silently retry an unknown failure).
///
/// The quota-before-credential order matters: a `429` body sometimes also
/// mentions the word "key", and a quota stall is the recoverable one we
/// most want to catch (the motivating finding).
///
/// # The `codex` special case
///
/// `codex` is the Adapter the finding named: it returns `1` for both
/// malformed prompts and over-quota refusals. Its branch widens the marker
/// set (`usage limit`, `rate_limit`, `529`, …) so the recoverable
/// quota-stall is rescued from the exit-`1` bucket instead of being
/// escalated as a dead-on-arrival user error.
#[must_use]
pub fn classify_exit(adapter: &str, raw_code: Option<i32>, stderr_tail: &str) -> AdapterExitClass {
    let Some(code) = raw_code else {
        // No exit code at all → the process was signalled or never ran.
        return AdapterExitClass::SpawnError;
    };

    if code == 0 {
        return AdapterExitClass::Ok;
    }

    // An Adapter that already speaks the contract is trusted verbatim.
    if let Some(class) = AdapterExitClass::from_code(code) {
        if !matches!(class, AdapterExitClass::UserError) {
            // 64/65/66 are unambiguous structured codes — believe them.
            return class;
        }
        // code == 1 falls through to stderr disambiguation below.
    }

    let lower = stderr_tail.to_ascii_lowercase();
    if stderr_signals_quota(&lower, adapter) {
        return AdapterExitClass::QuotaError;
    }
    if stderr_signals_credential(&lower, adapter) {
        return AdapterExitClass::CredentialError;
    }

    // Unknown non-zero code with no recognised marker: the conservative,
    // non-retryable verdict. Escalates rather than burning a retry budget
    // on a failure cosmon cannot characterise.
    AdapterExitClass::UserError
}

/// Common quota / rate-limit markers shared by every Adapter, widened with
/// per-Adapter extras.
///
/// `stderr` must already be lowercased by the caller.
fn stderr_signals_quota(stderr: &str, adapter: &str) -> bool {
    const COMMON: &[&str] = &[
        "quota",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "429",
        "usage limit",
        "over capacity",
    ];
    if COMMON.iter().any(|m| stderr.contains(m)) {
        return true;
    }
    // codex surfaces server-overload as `529` and "usage limit reached".
    if adapter == "codex" {
        const CODEX_EXTRA: &[&str] = &["529", "usage limit reached", "capacity"];
        return CODEX_EXTRA.iter().any(|m| stderr.contains(m));
    }
    false
}

/// Common credential / auth markers shared by every Adapter.
///
/// `stderr` must already be lowercased by the caller.
fn stderr_signals_credential(stderr: &str, _adapter: &str) -> bool {
    const COMMON: &[&str] = &[
        "401",
        "403",
        "unauthorized",
        "unauthenticated",
        "invalid api key",
        "invalid_api_key",
        "no api key",
        "missing api key",
        "authentication",
        "credential",
        "not logged in",
        "expired token",
    ];
    COMMON.iter().any(|m| stderr.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::WorkerId;

    #[test]
    fn code_round_trips_through_from_code() {
        for class in [
            AdapterExitClass::Ok,
            AdapterExitClass::UserError,
            AdapterExitClass::CredentialError,
            AdapterExitClass::QuotaError,
            AdapterExitClass::SpawnError,
        ] {
            assert_eq!(AdapterExitClass::from_code(class.code()), Some(class));
        }
    }

    #[test]
    fn canonical_codes_pin_the_contract() {
        // These exact integers are the wire contract a shell-wrapper
        // Adapter emits — a silent renumbering must fail here.
        assert_eq!(AdapterExitClass::Ok.code(), 0);
        assert_eq!(AdapterExitClass::UserError.code(), 1);
        assert_eq!(AdapterExitClass::CredentialError.code(), 64);
        assert_eq!(AdapterExitClass::QuotaError.code(), 65);
        assert_eq!(AdapterExitClass::SpawnError.code(), 66);
    }

    #[test]
    fn from_code_rejects_unmapped_integers() {
        assert_eq!(AdapterExitClass::from_code(2), None);
        assert_eq!(AdapterExitClass::from_code(127), None);
        assert_eq!(AdapterExitClass::from_code(-1), None);
    }

    #[test]
    fn wire_format_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&AdapterExitClass::Ok).unwrap(),
            r#""ok""#
        );
        assert_eq!(
            serde_json::to_string(&AdapterExitClass::CredentialError).unwrap(),
            r#""credential_error""#
        );
        assert_eq!(
            serde_json::to_string(&AdapterExitClass::QuotaError).unwrap(),
            r#""quota_error""#
        );
        assert_eq!(
            serde_json::to_string(&AdapterExitClass::SpawnError).unwrap(),
            r#""spawn_error""#
        );
    }

    #[test]
    fn retryable_partitions_the_alphabet() {
        assert!(AdapterExitClass::QuotaError.is_retryable());
        assert!(AdapterExitClass::SpawnError.is_retryable());
        assert!(!AdapterExitClass::Ok.is_retryable());
        assert!(!AdapterExitClass::UserError.is_retryable());
        assert!(!AdapterExitClass::CredentialError.is_retryable());
    }

    #[test]
    fn recovery_maps_each_class() {
        assert_eq!(AdapterExitClass::Ok.recovery(), RecoveryAction::None);
        assert_eq!(
            AdapterExitClass::UserError.recovery(),
            RecoveryAction::EscalateToOperator
        );
        assert_eq!(
            AdapterExitClass::CredentialError.recovery(),
            RecoveryAction::FixCredentials
        );
        assert_eq!(
            AdapterExitClass::QuotaError.recovery(),
            RecoveryAction::BackoffAndRetry
        );
        assert_eq!(
            AdapterExitClass::SpawnError.recovery(),
            RecoveryAction::Respawn
        );
    }

    // --- The motivating caae finding: codex exit 1 ambiguity ---

    #[test]
    fn codex_exit_one_malformed_prompt_is_user_error() {
        let class = classify_exit("codex", Some(1), "error: failed to parse prompt template");
        assert_eq!(class, AdapterExitClass::UserError);
        assert!(!class.is_retryable());
    }

    #[test]
    fn codex_exit_one_over_quota_is_quota_error() {
        // The exact case the finding describes: same exit 1, but stderr
        // reveals the recoverable quota stall.
        let class = classify_exit(
            "codex",
            Some(1),
            "stream error: 429 Too Many Requests — usage limit reached",
        );
        assert_eq!(class, AdapterExitClass::QuotaError);
        assert!(class.is_retryable());
    }

    #[test]
    fn codex_exit_one_server_overload_529_is_quota_error() {
        let class = classify_exit("codex", Some(1), "upstream returned 529 over capacity");
        assert_eq!(class, AdapterExitClass::QuotaError);
    }

    #[test]
    fn exit_one_invalid_key_is_credential_error() {
        let class = classify_exit("claude", Some(1), "401 Unauthorized: invalid api key");
        assert_eq!(class, AdapterExitClass::CredentialError);
    }

    #[test]
    fn structured_codes_are_trusted_without_stderr() {
        // An Adapter that already maps emits 64/65/66 directly; we believe
        // it even with empty stderr.
        assert_eq!(
            classify_exit("aider", Some(64), ""),
            AdapterExitClass::CredentialError
        );
        assert_eq!(
            classify_exit("aider", Some(65), ""),
            AdapterExitClass::QuotaError
        );
        assert_eq!(
            classify_exit("aider", Some(66), ""),
            AdapterExitClass::SpawnError
        );
    }

    #[test]
    fn signal_kill_is_spawn_error() {
        assert_eq!(
            classify_exit("codex", None, ""),
            AdapterExitClass::SpawnError
        );
    }

    #[test]
    fn clean_exit_is_ok() {
        assert_eq!(classify_exit("codex", Some(0), ""), AdapterExitClass::Ok);
    }

    #[test]
    fn unknown_nonzero_with_no_marker_is_user_error() {
        // Conservative default: never silently retry a failure cosmon
        // cannot characterise.
        let class = classify_exit("claude", Some(2), "segmentation fault");
        assert_eq!(class, AdapterExitClass::UserError);
    }

    #[test]
    fn quota_beats_credential_when_both_markers_present() {
        // A 429 body that also mentions "key" must classify as the
        // recoverable quota stall, not credential.
        let class = classify_exit(
            "openai",
            Some(1),
            "429 rate limit on your api key — retry later",
        );
        assert_eq!(class, AdapterExitClass::QuotaError);
    }

    #[test]
    fn to_patrol_action_retryable_restarts_worker() {
        let wid = WorkerId::new("w-1").unwrap();
        let action = AdapterExitClass::QuotaError.to_patrol_action(wid.clone());
        match action {
            PatrolAction::RestartWorker { worker_id, reason } => {
                assert_eq!(worker_id, wid);
                assert!(reason.contains("backoff-and-retry"), "reason: {reason}");
            }
            other => panic!("expected RestartWorker, got {other:?}"),
        }
    }

    #[test]
    fn to_patrol_action_user_error_alerts_human() {
        let wid = WorkerId::new("w-2").unwrap();
        let action = AdapterExitClass::UserError.to_patrol_action(wid);
        match action {
            PatrolAction::AlertHuman { message } => {
                assert!(message.contains("escalate-to-operator"), "msg: {message}");
                assert!(
                    message.contains('1'),
                    "msg should carry the code: {message}"
                );
            }
            other => panic!("expected AlertHuman, got {other:?}"),
        }
    }

    #[test]
    fn to_patrol_action_ok_is_no_action() {
        let wid = WorkerId::new("w-3").unwrap();
        assert_eq!(
            AdapterExitClass::Ok.to_patrol_action(wid),
            PatrolAction::NoAction
        );
    }
}
