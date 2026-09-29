// SPDX-License-Identifier: AGPL-3.0-only

use cosmon_core::{
    id::WorkerId,
    plan_observation::{claude_plan, PlanObservationStore, PlanSource},
};
use cosmon_state::plan_observation::{claude_statusline_overlay, FilePlanObservationStore};
use serde_json::json;

#[test]
fn configuration_is_preserved_and_unknown_effective_settings_refused() {
    let existing = json!({"statusLine":{"type":"command","command":"custom --flag 'hello world'","padding":3},"hooks":{"Stop":[{"unchanged":true}]},"permissions":{"defaultMode":"default"}});
    let before = existing.clone();
    let overlay = claude_statusline_overlay(Some(&existing), "collector").expect("compose");
    assert_eq!(existing, before);
    assert_eq!(overlay["statusLine"]["padding"], 3);
    assert_eq!(
        overlay["statusLine"]["command"],
        "collector | (\ncustom --flag 'hello world'\n)"
    );
    assert!(overlay.get("hooks").is_none());
    assert!(claude_statusline_overlay(None, "collector").is_err());
    assert!(
        claude_statusline_overlay(Some(&json!({"statusLine":{"type":"other"}})), "collector")
            .is_err()
    );
}

#[test]
fn concurrent_workers_and_out_of_order_writes_preserve_sanitized_latest_samples() {
    let tmp = tempfile::tempdir().expect("temp");
    let root = tmp.path().to_owned();
    let now: chrono::DateTime<chrono::Utc> = "2026-09-28T12:00:00Z".parse().expect("time");
    std::thread::scope(|scope| {
        for index in 0..12 {
            let root = root.clone();
            scope.spawn(move || {
                let store = FilePlanObservationStore::new(root);
                let worker = WorkerId::new(format!("worker-{}", index % 2)).expect("worker");
                let sample = claude_plan(&format!(r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{index}}}}},"private":"must-not-persist"}}"#), PlanSource::ClaudeStatusLine, now + chrono::Duration::seconds(index));
                store.save(&worker, &sample).expect("save");
            });
        }
    });
    let store = FilePlanObservationStore::new(root.clone());
    for (name, expected) in [("worker-0", 0.10), ("worker-1", 0.11)] {
        let worker = WorkerId::new(name).expect("worker");
        let old = claude_plan(
            r#"{"rate_limits":{"five_hour":{"used_percentage":1}}}"#,
            PlanSource::ClaudeStatusLine,
            now - chrono::Duration::seconds(1),
        );
        store.save(&worker, &old).expect("late older write");
        let sample = store
            .load(&worker, PlanSource::ClaudeStatusLine)
            .expect("load")
            .expect("sample");
        assert_eq!(sample.plan.windows[0].utilization.get(), expected);
        assert_eq!(
            store
                .load(&worker, PlanSource::ClaudeStatusLine)
                .expect("reload"),
            Some(sample)
        );
    }
    for entry in std::fs::read_dir(root).expect("list") {
        let data = std::fs::read_to_string(entry.expect("entry").path()).expect("bytes");
        assert!(!data.contains("must-not-persist"));
    }
}

#[test]
fn read_missing_store_is_side_effect_free() {
    let tmp = tempfile::tempdir().expect("temp");
    let root = tmp.path().join("absent");
    let store = FilePlanObservationStore::new(root.clone());
    assert!(store
        .load(
            &WorkerId::new("reader").expect("worker"),
            PlanSource::ClaudeStatusLine
        )
        .expect("read")
        .is_none());
    assert!(!root.exists());
}

#[test]
fn reused_worker_name_starts_without_the_previous_reading() {
    let tmp = tempfile::tempdir().expect("temp");
    let store = FilePlanObservationStore::new(tmp.path().join("samples"));
    let worker = WorkerId::new("quartz").expect("worker");
    let sample = claude_plan(
        r#"{"rate_limits":{"five_hour":{"used_percentage":42}}}"#,
        PlanSource::ClaudeStatusLine,
        chrono::Utc::now(),
    );
    store.save(&worker, &sample).expect("save");
    store
        .retire(&worker, PlanSource::ClaudeStatusLine)
        .expect("retire");
    assert!(store
        .load(&worker, PlanSource::ClaudeStatusLine)
        .expect("load")
        .is_none());
}
