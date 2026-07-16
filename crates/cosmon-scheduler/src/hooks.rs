// SPDX-License-Identifier: AGPL-3.0-only

//! Sunset hook dispatcher — feature-flagged side-effects that run *after*
//! the scheduler has flipped `sunset_decided_at` and emitted the
//! `patrol.sunsetted` event.
//!
//! ## Why a separate module
//!
//! `dispatch.rs` owns the `launchctl unload` side-effect (advisory,
//! directly coupled to the plist). Hooks are different: they are
//! **opt-in notifications** declared by the operator in `on_sunset = […]`
//! and resolved at tick time against environment variables. Keeping them
//! here preserves `dispatch.rs`'s focus on "what does `Patrol.command`
//! expand to" and keeps the hook matrix independently unit-testable.
//!
//! ## Two hooks, same fail-open discipline
//!
//! | Hook                   | Feature flag                       | Outcome on failure |
//! |------------------------|------------------------------------|--------------------|
//! | `notify_telegram`      | `COSMON_TELEGRAM_HOOK_SCRIPT` env  | log `patrol.sunset_hook_failed`, continue |
//! | `write_chronicle_stub` | `COSMON_CHRONICLE_FILE` env        | log `patrol.sunset_hook_failed`, continue |
//!
//! Both hooks are **no-ops when their flag env var is unset**. A patrol
//! declaring `on_sunset = ["notify_telegram"]` on a machine where
//! `COSMON_TELEGRAM_HOOK_SCRIPT` is not set emits a silent "skipped"
//! outcome — not an error. That is how we ship a hook list in TOML that
//! travels across machines without tripping CI on a laptop without
//! credentials.
//!
//! ## Telegram payload shape
//!
//! The Telegram hook pipes a single NDJSON line on the child's stdin,
//! shaped to match the contract of `hooks/telegram-notify.sh` already
//! shipped in the repo:
//!
//! ```json
//! {"timestamp":"2026-04-19T15:00:00Z","kind":"patrol_sunsetted","patrol":"u2-probe","reason":"variance-threshold converged"}
//! ```
//!
//! `kind = "patrol_sunsetted"` (underscore) matches the bash script's
//! `case "$kind" in …` convention; the scheduler event log uses
//! `"patrol.sunsetted"` (dot) because that is our internal namespace
//! convention. The two vocabularies are translated here so the bash
//! script needs no change. The fallback `*)` branch in the script still
//! renders the event as a pre-formatted block if the operator has not
//! yet added a `patrol_sunsetted)` case — we are fail-open there too.
//!
//! ## Chronicle-stub shape
//!
//! The chronicle hook appends a Feynman-register one-liner stub to the
//! file pointed at by `COSMON_CHRONICLE_FILE` (usually
//! the galaxy's chronicle file). The line carries the date, patrol name,
//! and reason — enough for the operator to later expand it into a full
//! entry. The file must already exist and be a plain text file; the hook
//! appends, never creates.
//!
//! ## Why env-var feature flags instead of TOML fields
//!
//! The operator declares *intent* in `patrols.toml` (`on_sunset =
//! ["notify_telegram"]`). The environment declares *capability*
//! (`TELEGRAM_BOT_TOKEN`, `COSMON_CHRONICLE_FILE`). Splitting the two
//! along that seam lets the same TOML travel unchanged between a laptop
//! (no Telegram) and a dedicated host (has Telegram) without duplicating
//! the declaration. It is the same discipline `require_env` already uses
//! for the cadence gate.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Patrol;
use crate::environment::shellexpand_home;

/// Outcome of one hook invocation. Advisory — the caller emits a
/// `patrol.sunset_hook_failed` event when `error` is `Some` but does
/// **not** roll back `sunset_decided_at`. Hooks are notifications, not
/// state transitions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookOutcome {
    /// Hook name as it appears in `on_sunset = [...]`. Preserved verbatim
    /// so operator-facing logs match the TOML source.
    pub name: String,

    /// `None` on success *or* graceful skip (hook disabled by env flag).
    /// `Some(detail)` on real failure (script exited non-zero, file
    /// unreachable, etc.). The caller discriminates "skip" vs "run" by
    /// looking at `status`.
    pub error: Option<String>,

    /// Was the hook actually executed, or skipped because its feature
    /// flag was unset? Operators typically filter `HookStatus::Ran`
    /// when reporting to a dashboard.
    pub status: HookStatus,
}

/// Tri-state outcome for a hook invocation. Distinguishes a healthy skip
/// (env unset) from a ran-then-failed from a ran-cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// Hook ran to completion with no error.
    Ran,
    /// Hook ran but returned a failure (non-zero exit, I/O error).
    Failed,
    /// Hook was declared but its feature flag was unset — no work done.
    Skipped,
    /// Hook name was declared but unknown to the scheduler.
    Unknown,
}

/// Run every `on_sunset` hook declared by `patrol`, in declaration
/// order. Returns one [`HookOutcome`] per declared hook — including
/// skipped and unknown hooks — so the caller can log the full picture.
///
/// `reason` is the human-readable explanation captured in
/// `Decision::WouldSunset`, reused verbatim for the Telegram body and
/// the chronicle stub.
///
/// Pure wrt state: no scheduler-state fields are written here. A hook
/// failure is an *event*, not a state transition.
#[must_use]
pub fn run_sunset_hooks(patrol: &Patrol, now: DateTime<Utc>, reason: &str) -> Vec<HookOutcome> {
    let Some(sunset) = patrol.sunset.as_ref() else {
        return Vec::new();
    };
    sunset
        .on_sunset
        .iter()
        .map(|name| run_one_hook(name, &patrol.name, now, reason))
        .collect()
}

fn run_one_hook(name: &str, patrol_name: &str, now: DateTime<Utc>, reason: &str) -> HookOutcome {
    match name {
        "notify_telegram" => run_notify_telegram(name, patrol_name, now, reason),
        "write_chronicle_stub" => run_write_chronicle_stub(name, patrol_name, now, reason),
        // `unload_launchd` is not a hook here — it is handled by
        // `run_sunset_action` in `dispatch.rs` which reads the
        // `launchctl_plist` field directly. Declaring it in
        // `on_sunset` as well is a redundant-but-harmless alias.
        "unload_launchd" => HookOutcome {
            name: name.to_owned(),
            error: None,
            status: HookStatus::Skipped,
        },
        _ => HookOutcome {
            name: name.to_owned(),
            error: Some(format!("unknown hook '{name}'")),
            status: HookStatus::Unknown,
        },
    }
}

fn run_notify_telegram(
    hook_name: &str,
    patrol_name: &str,
    now: DateTime<Utc>,
    reason: &str,
) -> HookOutcome {
    let Some(script_raw) = std::env::var_os("COSMON_TELEGRAM_HOOK_SCRIPT") else {
        return HookOutcome {
            name: hook_name.to_owned(),
            error: None,
            status: HookStatus::Skipped,
        };
    };
    let script_path = PathBuf::from(script_raw);
    let payload = serde_json::json!({
        "timestamp": now.to_rfc3339(),
        "kind": "patrol_sunsetted",
        "patrol": patrol_name,
        "reason": reason,
    })
    .to_string();

    match spawn_and_pipe(&script_path, &payload) {
        Ok(()) => HookOutcome {
            name: hook_name.to_owned(),
            error: None,
            status: HookStatus::Ran,
        },
        Err(e) => HookOutcome {
            name: hook_name.to_owned(),
            error: Some(e),
            status: HookStatus::Failed,
        },
    }
}

fn spawn_and_pipe(script: &Path, payload: &str) -> Result<(), String> {
    let mut child = Command::new(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", script.display()))?;

    // Scope the stdin handle so it closes (and flushes) before we wait.
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "child stdin unavailable".to_owned())?;
        stdin
            .write_all(payload.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .map_err(|e| format!("write payload: {e}"))?;
    }

    // Bounded wait — the Telegram script has its own 10s curl timeout,
    // but we protect the scheduler against a runaway hook with a
    // generous ceiling so one stuck hook never delays the next tick.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(format!(
                    "script exited non-zero: {code}",
                    code = status.code().map_or("signal".to_owned(), |c| c.to_string())
                ));
            }
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    // Leak the child — its own timeout will clean it up.
                    return Err("timed out after 30s".to_owned());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

fn run_write_chronicle_stub(
    hook_name: &str,
    patrol_name: &str,
    now: DateTime<Utc>,
    reason: &str,
) -> HookOutcome {
    let Some(raw) = std::env::var_os("COSMON_CHRONICLE_FILE") else {
        return HookOutcome {
            name: hook_name.to_owned(),
            error: None,
            status: HookStatus::Skipped,
        };
    };
    let raw_lossy = raw.to_string_lossy();
    let expanded = shellexpand_home(raw_lossy.as_ref()).into_owned();
    let path = PathBuf::from(expanded);

    match append_chronicle_stub(&path, patrol_name, now, reason) {
        Ok(()) => HookOutcome {
            name: hook_name.to_owned(),
            error: None,
            status: HookStatus::Ran,
        },
        Err(e) => HookOutcome {
            name: hook_name.to_owned(),
            error: Some(e),
            status: HookStatus::Failed,
        },
    }
}

fn append_chronicle_stub(
    path: &Path,
    patrol_name: &str,
    now: DateTime<Utc>,
    reason: &str,
) -> Result<(), String> {
    use std::fs::OpenOptions;

    let mut f = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let stub = format!(
        "\n- **{date} — patrol `{patrol}` sunsetted.** {reason}\n",
        date = now.date_naive(),
        patrol = patrol_name,
        reason = reason
    );
    f.write_all(stub.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Sunset, SunsetStrategy};
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap()
    }

    fn patrol_with_hooks(hooks: Vec<&str>) -> Patrol {
        Patrol {
            name: "u2-probe".to_owned(),
            interval_seconds: Some(300),
            cron: None,
            command: vec!["echo".to_owned()],
            working_dir: None,
            env: BTreeMap::new(),
            kill_switch: None,
            log_file: None,
            dispatch: "detached".to_owned(),
            require_env: Vec::new(),
            timeout_seconds: None,
            enabled: true,
            sunset: Some(Sunset {
                strategy: SunsetStrategy::VarianceThreshold,
                sample_file: Some("/tmp/s.tsv".to_owned()),
                min_samples: Some(30),
                variance_threshold: Some(0.02),
                window: Some(20),
                trigger_file: None,
                launchctl_plist: None,
                on_sunset: hooks.into_iter().map(str::to_owned).collect(),
            }),
        }
    }

    #[test]
    fn no_sunset_block_means_no_hooks() {
        let mut p = patrol_with_hooks(Vec::new());
        p.sunset = None;
        let outcomes = run_sunset_hooks(&p, fixed_now(), "unused");
        assert!(outcomes.is_empty());
    }

    #[test]
    fn empty_hook_list_returns_empty() {
        let p = patrol_with_hooks(Vec::new());
        let outcomes = run_sunset_hooks(&p, fixed_now(), "unused");
        assert!(outcomes.is_empty());
    }

    #[test]
    fn unknown_hook_reports_unknown_status() {
        let p = patrol_with_hooks(vec!["nope"]);
        let outcomes = run_sunset_hooks(&p, fixed_now(), "r");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].name, "nope");
        assert_eq!(outcomes[0].status, HookStatus::Unknown);
        assert!(outcomes[0].error.as_deref().unwrap().contains("unknown"));
    }

    #[test]
    fn unload_launchd_is_noop_alias() {
        let p = patrol_with_hooks(vec!["unload_launchd"]);
        let outcomes = run_sunset_hooks(&p, fixed_now(), "r");
        assert_eq!(outcomes[0].status, HookStatus::Skipped);
        assert!(outcomes[0].error.is_none());
    }

    // Note: the "feature-flag absent => Skipped" path is tested by the
    // build-and-ship convention (CI does not export these env vars), not
    // by unit tests: Rust 2024 made `env::remove_var` unsafe and this
    // crate forbids unsafe code. The hook contract — "env unset ⇒ skip,
    // error-free" — is visible in the function body and in the
    // `run_sunset_hooks` caller; a regression would surface loudly in
    // `dry_run.rs` or in production as an unexpected "Ran" outcome on
    // a laptop that was supposed to stay silent.

    #[test]
    fn chronicle_stub_appends_feynman_line_when_env_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("CHRONICLES.md");
        std::fs::write(&path, "# Chronicles\n\nPreamble.\n").unwrap();

        append_chronicle_stub(&path, "u2-probe", fixed_now(), "σ² < 0.02").expect("append ok");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("Preamble."));
        assert!(body.contains("`u2-probe`"));
        assert!(body.contains("2026-04-19"));
        assert!(body.contains("σ² < 0.02"));
    }

    #[test]
    fn chronicle_stub_reports_error_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.md");
        let err = append_chronicle_stub(&path, "u2-probe", fixed_now(), "r")
            .expect_err("missing file is an error");
        assert!(err.contains("open"), "got: {err}");
    }

    #[test]
    fn hook_outcome_is_json_serializable() {
        let o = HookOutcome {
            name: "notify_telegram".to_owned(),
            error: None,
            status: HookStatus::Ran,
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("ran"), "got: {json}");
        let back: HookOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
    }
}
