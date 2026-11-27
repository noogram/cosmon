// SPDX-License-Identifier: AGPL-3.0-only

//! Provider account allowance through the built `cs` binary (issue #242).
//!
//! A Codex worker's recorded rollout (`fixtures/codex/rate_limits.jsonl`,
//! synthetic ids) sits in the molecule-owned Codex home, where `cs tackle`
//! points a Codex worker. The rollout's times are written relative to the
//! test clock: the reading is three minutes old and falls 42 hours into a
//! seven-day window, at 50 % used, so the expected pace is 25 % in deficit,
//! running out 42 hours after the reading.

use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
use serde_json::{json, Value};

const ROLLOUT: &str = include_str!("fixtures/codex/rate_limits.jsonl");
const MOLECULE: &str = "task-20261127-a242";
const WORKER: &str = "worker-codex-accounts";

/// When the recorded reading was made, relative to now.
struct Reading {
    age: Duration,
    /// Time from the window's start to the reading.
    into_window: Duration,
    /// Whether the rollout carries rate limits at all.
    rate_limits: bool,
}

impl Reading {
    const FRESH: Self = Self {
        age: Duration::minutes(3),
        into_window: Duration::hours(42),
        rate_limits: true,
    };
}

fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn seed(root: &Path, reading: &Reading) -> anyhow::Result<()> {
    let state = root.join(".cosmon/state");
    fs::create_dir_all(&state)?;
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"peek-accounts-test\"\n",
    )?;
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree)?;
    let worktree = worktree.canonicalize()?;

    let store = FileStore::new(&state);
    let id = MoleculeId::new(MOLECULE)?;
    let worker_id = WorkerId::new(WORKER)?;
    let now = Utc::now();
    let worker: WorkerData = serde_json::from_value(json!({
        "id": worker_id, "agent_id": "agent-test", "role": "implementation",
        "clearance": "execute", "status": "active", "current_molecule": id,
        "updated_at": now, "repo": worktree,
    }))?;
    let molecule: MoleculeData = serde_json::from_value(json!({
        "id": id, "formula_id": "task-work", "status": "running", "variables": {},
        "assigned_worker": worker_id, "created_at": now, "updated_at": now,
        "total_steps": 1, "current_step": 0, "completed_steps": [], "links": [],
        "project_id": "peek-accounts-test"
    }))?;
    let mut fleet = Fleet::new();
    fleet.workers.insert(worker_id, worker);
    store.save_fleet(&fleet)?;
    store.save_molecule(&id, &molecule)?;
    cosmon_state::event_log::emit_one(
        &cosmon_state::event_log::resolve_events_log_path(&state),
        EventV2::AdapterSelected {
            mol_id: id.clone(),
            adapter_name: "codex".to_owned(),
            selected_at: now,
            selection_source: AdapterSelectionSource::Cli {
                flag: "codex".to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )?;

    let observed = now - reading.age;
    let resets_at = observed - reading.into_window + Duration::days(7);
    let mut rollout = ROLLOUT
        .replace("{{CWD}}", &worktree.to_string_lossy())
        .replace("{{STARTED}}", &stamp(observed - Duration::hours(2)))
        .replace("{{EARLIER}}", &stamp(observed - Duration::hours(1)))
        .replace("{{OBSERVED}}", &stamp(observed))
        .replace("{{RESETS_AT}}", &resets_at.timestamp().to_string());
    if !reading.rate_limits {
        rollout = rollout
            .lines()
            .map(|line| match line.find(",\"rate_limits\":{") {
                Some(at) => format!("{},\"rate_limits\":null}}}}", &line[..at]),
                None => line.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let sessions = store
        .molecule_dir(&id)
        .join("codex-work-home/sessions/2026/11/27");
    fs::create_dir_all(&sessions)?;
    fs::write(sessions.join("rollout-fixture-242.jsonl"), rollout)?;
    Ok(())
}

fn cs(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(root)
        .env("NO_COLOR", "1")
        // Keep pulse off the operator's scheduler files.
        .env("COSMON_SCHED_LOG", root.join("absent-sched.log"))
        .env("COSMON_SCHED_STATE", root.join("absent-sched.json"))
        .args(args)
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn peek_accounts(root: &Path) -> anyhow::Result<Value> {
    let output = cs(root, &["--json", "peek"])?;
    let doc: Value = serde_json::from_str(&output)?;
    Ok(doc["accounts"].clone())
}

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

fn near(value: &Value, expected: i64, slack: i64) -> bool {
    value
        .as_i64()
        .is_some_and(|v| (v - expected).abs() <= slack)
}

#[test]
fn peek_json_reports_allowance_reset_pace_credits_and_worker_share() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), &Reading::FRESH)?;
    let accounts = peek_accounts(tmp.path())?;
    assert_eq!(accounts.as_array().map(Vec::len), Some(1), "{accounts:#}");
    let account = &accounts[0];
    assert_eq!(account["provider"], "openai", "{account:#}");
    assert_eq!(account["window_minutes"], 10_080);
    assert!((number(&account["used_percent"]) - 50.0).abs() < 1e-9);
    assert!((number(&account["left_percent"]) - 50.0).abs() < 1e-9);
    assert_eq!(account["freshness"], "fresh");
    assert!(near(&account["age_secs"], 180, 30), "{account:#}");
    // 126 h from the reading, three minutes of which have passed.
    assert!(
        near(&account["resets_in_secs"], 126 * 3600 - 180, 30),
        "{account:#}"
    );
    let pace = &account["pace"];
    assert_eq!(pace["status"], "known", "{account:#}");
    assert!((number(&pace["even_use_percent"]) - 25.0).abs() < 1e-6);
    assert!((number(&pace["deficit_percent"]) - 25.0).abs() < 1e-6);
    assert!(
        near(&pace["runs_out_in_secs"], 42 * 3600 - 180, 30),
        "{account:#}"
    );
    assert_eq!(account["credits"]["balance"], 0.0);
    assert_eq!(account["credits"]["has_credits"], false);
    assert_eq!(account["conflicting_readings"], false);
    assert_eq!(
        account["workers"],
        json!([{"worker_id": WORKER, "tokens": 35_000, "share_percent": 100.0}]),
        "{account:#}"
    );
    Ok(())
}

#[test]
fn peek_no_tui_prints_one_line_per_account_with_the_worker_shares() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), &Reading::FRESH)?;
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(
        text.contains(
            "account openai 7d: 50% left · resets in 5d 5h · 25% in deficit, \
             runs out in 1d 17h · credits 0 · seen 3m ago"
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "workers: {WORKER} 100% (uncached input + output tokens)"
        )),
        "{text}"
    );
    Ok(())
}

#[test]
fn pulse_json_publishes_the_same_account_fields_as_peek() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), &Reading::FRESH)?;
    let from_peek = peek_accounts(tmp.path())?;
    let output = cs(tmp.path(), &["pulse", "--json"])?;
    let from_pulse = serde_json::from_str::<Value>(&output)?["accounts"].clone();
    // The two commands read the clock a moment apart; strip the figures
    // measured from now and compare everything else exactly.
    let timeless = |accounts: &Value| {
        let mut accounts = accounts.clone();
        for account in accounts.as_array_mut().into_iter().flatten() {
            account["age_secs"].take();
            account["resets_in_secs"].take();
            account["pace"]["runs_out_in_secs"].take();
            account["freshness"].take();
        }
        accounts
    };
    assert_eq!(timeless(&from_pulse), timeless(&from_peek), "{output}");
    assert_eq!(from_pulse[0]["freshness"], "fresh", "{output}");
    Ok(())
}

#[test]
fn an_old_reading_is_published_as_stale_with_its_age() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(
        tmp.path(),
        &Reading {
            age: Duration::hours(2),
            ..Reading::FRESH
        },
    )?;
    let accounts = peek_accounts(tmp.path())?;
    assert_eq!(accounts[0]["freshness"], "stale", "{accounts:#}");
    assert!(near(&accounts[0]["age_secs"], 7200, 30), "{accounts:#}");
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("stale, seen 2h 0m ago"), "{text}");
    Ok(())
}

#[test]
fn a_reading_from_a_window_that_has_reset_leaves_the_allowance_unknown() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    // The reading was made 6 d 4 h into its window, a day ago: the window
    // reset four hours ago and nothing has been read since.
    seed(
        tmp.path(),
        &Reading {
            age: Duration::days(1),
            into_window: Duration::hours(148),
            rate_limits: true,
        },
    )?;
    let accounts = peek_accounts(tmp.path())?;
    let account = &accounts[0];
    assert_eq!(account["freshness"], "reset", "{account:#}");
    assert_eq!(account["left_percent"], Value::Null, "{account:#}");
    assert_eq!(account["pace"]["status"], "unknown");
    assert_eq!(account["pace"]["reason"], "window_reset");
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(
        text.contains("account openai 7d: reset 4h 0m ago, not observed since"),
        "{text}"
    );
    Ok(())
}

#[test]
fn a_rollout_without_rate_limits_reports_no_account_rather_than_zero() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(
        tmp.path(),
        &Reading {
            rate_limits: false,
            ..Reading::FRESH
        },
    )?;
    let output = cs(tmp.path(), &["--json", "peek"])?;
    let doc: Value = serde_json::from_str(&output)?;
    // Same worker and rollout as the other tests, which do see the account:
    // only the rate limits are missing, so the account is absent, not zero.
    assert_eq!(doc["accounts"], json!([]), "{output}");
    let text = cs(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(!text.contains("account "), "{text}");
    Ok(())
}
