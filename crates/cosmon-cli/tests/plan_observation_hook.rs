// SPDX-License-Identifier: AGPL-3.0-only

use cosmon_core::{
    id::WorkerId,
    plan_observation::{PlanObservationStore, PlanSource},
};
use cosmon_state::plan_observation::{claude_statusline_overlay, FilePlanObservationStore};
use serde_json::json;
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn hook_preserves_existing_display_and_input_even_if_storage_fails() {
    let temp = tempfile::tempdir().expect("temp");
    let blocked = temp.path().join("not-directory");
    std::fs::write(&blocked, "block").expect("seed");
    for root in [temp.path().join("samples"), blocked] {
        let collector = format!(
            "'{}' plan-observation-hook '{}' witness",
            env!("CARGO_BIN_EXE_cs"),
            root.display()
        );
        let settings = json!({"statusLine":{"type":"command","command":"cat; printf 'original-output' # preserve comment","padding":2}});
        let overlay = claude_statusline_overlay(Some(&settings), &collector).expect("compose");
        let input = br#"{"rate_limits":{"five_hour":{"used_percentage":42}},"private":"keep-only-in-pipe"}"#;
        let mut child = Command::new("sh")
            .args([
                "-c",
                overlay["statusLine"]["command"].as_str().expect("command"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("write");
        let output = child.wait_with_output().expect("wait");
        assert!(output.status.success());
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        assert_eq!(
            output.stdout,
            [input.as_slice(), b"original-output"].concat()
        );
        if root.is_dir() {
            let sample = FilePlanObservationStore::new(root)
                .load(
                    &WorkerId::new("witness").expect("worker"),
                    PlanSource::ClaudeStatusLine,
                )
                .expect("load")
                .expect("sample");
            assert_eq!(sample.plan.windows[0].utilization.get(), 0.42);
            assert!(!serde_json::to_string(&sample)
                .expect("json")
                .contains("keep-only-in-pipe"));
        }
    }
}
