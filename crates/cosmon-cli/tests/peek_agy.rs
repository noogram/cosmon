// SPDX-License-Identifier: AGPL-3.0-only

//! Receipt-to-output contract for the built `cs peek` binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use cosmon_core::event_v2::{AdapterSelectionSource, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
use serde_json::{json, Value};

const QUOTA: &str = include_str!("fixtures/agy/quota.jsonl");
const TRUNCATED: &str = include_str!("fixtures/agy/truncated.jsonl");
const PARTIAL: &str = include_str!("fixtures/agy/partial.jsonl");

fn seed(root: &Path, receipt: &str) -> anyhow::Result<()> {
    let state = root.join(".cosmon/state");
    fs::create_dir_all(&state)?;
    fs::write(
        root.join(".cosmon/config.toml"),
        "[project]\nproject_id = \"peek-agy-test\"\n",
    )?;
    let store = FileStore::new(&state);
    let id = MoleculeId::new("task-20261126-a901")?;
    let worker_id = WorkerId::new("worker-agy-test")?;
    let now = Utc::now();
    let worker: WorkerData = serde_json::from_value(json!({
        "id": worker_id, "agent_id": "agent-test", "role": "implementation",
        "clearance": "execute", "status": "stopped", "current_molecule": id, "updated_at": now
    }))?;
    let molecule: MoleculeData = serde_json::from_value(json!({
        "id": id, "formula_id": "task-work", "status": "running", "variables": {},
        "assigned_worker": worker_id, "created_at": now, "updated_at": now,
        "total_steps": 1, "current_step": 0, "completed_steps": [], "links": [],
        "project_id": "peek-agy-test"
    }))?;
    let mut fleet = Fleet::new();
    fleet.workers.insert(worker_id, worker);
    store.save_fleet(&fleet)?;
    store.save_molecule(&id, &molecule)?;
    cosmon_state::event_log::emit_one(
        &cosmon_state::event_log::resolve_events_log_path(&state),
        EventV2::AdapterSelected {
            mol_id: id.clone(),
            adapter_name: "agy".to_owned(),
            selected_at: now,
            selection_source: AdapterSelectionSource::Cli {
                flag: "agy".to_owned(),
            },
            role_hint: None,
            loop_ownership: Default::default(),
        },
        None,
    )?;
    fs::write(store.molecule_dir(&id).join("agy-stream.jsonl"), receipt)?;
    Ok(())
}

fn cs(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(root).env("NO_COLOR", "1");
    cmd
}

fn peek(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = cs(root).args(args).output()?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

#[test]
fn agy_receipt_usage_is_projected_as_four_counts() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), QUOTA)?;
    let output = peek(tmp.path(), &["--json", "peek"])?;
    let doc: Value = serde_json::from_str(&output)?;
    let tokens = &doc["molecules"][0]["usage"][0]["tokens"];
    assert_eq!(tokens["input_tokens"]["tokens"], 528_109, "{output}");
    assert_eq!(tokens["cached_input_tokens"]["tokens"], 369_075);
    assert_eq!(tokens["output_tokens"]["tokens"], 8_486);
    assert_eq!(tokens["reasoning_output_tokens"]["tokens"], 4_698);
    assert_eq!(doc["molecules"][0]["agy_usage"]["input_tokens"], 159_034);
    let text = peek(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("159034/369075/8486/4698"), "{text}");
    let snapshot = peek(tmp.path(), &["peek", "--snapshot"])?;
    assert!(snapshot.contains("159.0K/369.0K/8.4K/4.6K"), "{snapshot}");
    assert!(
        snapshot.contains("quota-exhausted (resets in 19m45s)"),
        "{snapshot}"
    );
    Ok(())
}

#[test]
fn agy_quota_stop_includes_the_provider_delay() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), QUOTA)?;
    let text = peek(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
    assert!(text.contains("quota-exhausted"), "{text}");
    assert!(text.contains("19m45s"), "{text}");
    let output = peek(tmp.path(), &["--json", "peek"])?;
    let doc: Value = serde_json::from_str(&output)?;
    assert_eq!(doc["molecules"][0]["ghost"], "quota_exhausted", "{output}");
    assert_eq!(doc["molecules"][0]["quota_reset_delay"], "19m45s");
    Ok(())
}

#[test]
fn agy_receipt_distinguishes_unavailable_and_measured_zero() -> anyhow::Result<()> {
    for (receipt, expected) in [(TRUNCATED, "-/-/-/-"), (PARTIAL, "0/0/21/-")] {
        let tmp = tempfile::tempdir()?;
        seed(tmp.path(), receipt)?;
        let text = peek(tmp.path(), &["peek", "--no-tui", "--once", "--no-tmux"])?;
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains("quota-exhausted"), "{text}");
        let output = peek(tmp.path(), &["--json", "peek"])?;
        let doc: Value = serde_json::from_str(&output)?;
        let row = &doc["molecules"][0];
        assert!(row.get("ghost").is_none(), "{output}");
        if receipt == TRUNCATED {
            assert_eq!(row["usage"], json!([]), "{output}");
        } else {
            let tokens = &row["usage"][0]["tokens"];
            assert_eq!(tokens["input_tokens"]["tokens"], 0, "{output}");
            assert_eq!(
                tokens["reasoning_output_tokens"]["status"], "unavailable",
                "{output}"
            );
        }
    }
    Ok(())
}

#[test]
fn agy_usage_uses_the_existing_pulse_fuel_channel() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    seed(tmp.path(), QUOTA)?;
    let mut observer = cs(tmp.path())
        .args([
            "peek",
            "--no-tui",
            "--follow",
            "--no-tmux",
            "--energy-tick-interval",
            "1",
        ])
        .stdout(Stdio::null())
        .spawn()?;

    let journal = tmp.path().join(".cosmon/state/events.jsonl");
    let deadline = Instant::now() + Duration::from_secs(15);
    let recorded = (|| -> anyhow::Result<bool> {
        loop {
            if fs::read_to_string(&journal)?.contains("\"type\":\"usage_observed\"") {
                return Ok(true);
            }
            if Instant::now() >= deadline || observer.try_wait()?.is_some() {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    let _ = observer.kill();
    observer.wait()?;
    assert!(recorded?, "peek did not journal the agy observation");

    let output = peek(tmp.path(), &["pulse", "--json"])?;
    let doc: Value = serde_json::from_str(&output)?;
    let expected = 536_595.0 / 10_000_000.0;
    let fuel = doc["fuel_pct"].as_f64().expect("pulse fuel percentage");
    assert!((fuel - expected).abs() < 1e-10, "{output}");
    Ok(())
}
