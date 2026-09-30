// SPDX-License-Identifier: AGPL-3.0-only

//! Journal receipt to observe text and JSON, with and without a model pin.

use std::process::Command;

use cosmon_core::event_v2::{
    AdapterSelectionSource, EventV2, LoopOwnershipTag, ModelEvidenceGeneration,
    ModelSelectionSource,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::model_realization::{
    assess_claude_model_evidence, ModelEvidenceGrammar, ModelObservationSource,
};
use cosmon_state::event_log::{emit_one, resolve_events_log_path};

fn command(cwd: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(cwd)
        .env_remove("COSMON_STATE_DIR")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_ARTIFACT_DIR");
    cmd
}

#[test]
fn degraded_receipt_survives_observe_text_and_json_with_or_without_pin() {
    for pinned in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        std::fs::create_dir_all(&formulas_dir).unwrap();
        std::fs::write(
            formulas_dir.join("evidence.formula.toml"),
            "formula = \"evidence\"\nversion = 1\ndescription = \"Evidence\"\nid_prefix = \"evd\"\n[[steps]]\nid = \"only\"\ntitle = \"Only\"\ndescription = \"Only\"\nacceptance = \"done\"\n",
        )
        .unwrap();
        let nucleated = command(tmp.path())
            .args([
                "--json",
                "nucleate",
                "evidence",
                "--store-dir",
                state_dir.to_str().unwrap(),
                "--formulas-dir",
                formulas_dir.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            nucleated.status.success(),
            "{}",
            String::from_utf8_lossy(&nucleated.stderr)
        );
        let created: serde_json::Value = serde_json::from_slice(&nucleated.stdout).unwrap();
        let mol = MoleculeId::new(created["id"].as_str().unwrap()).unwrap();
        let worker = WorkerId::new("worker-evidence").unwrap();
        let log = resolve_events_log_path(&state_dir);
        let selected_at = chrono::Utc::now();
        let events = [
            EventV2::AdapterSelected {
                mol_id: mol.clone(),
                adapter_name: "claude".into(),
                selected_at,
                selection_source: AdapterSelectionSource::Cli { flag: "claude".into() },
                role_hint: None,
                loop_ownership: LoopOwnershipTag::default(),
            },
            EventV2::ModelSelected {
                mol_id: mol.clone(),
                adapter_name: "claude".into(),
                model: pinned.then(|| "model-a".into()),
                selection_source: if pinned {
                    ModelSelectionSource::Flag { flag: "model-a".into() }
                } else {
                    ModelSelectionSource::Default { fallback_reason: "floor".into() }
                },
                selected_at,
            },
            EventV2::WorkerSpawned {
                worker_id: worker.clone(),
                molecule: Some(mol.clone()),
                session_name: "example".into(),
                role: "worker".into(),
                adapter_name: "claude".into(),
                loop_ownership: LoopOwnershipTag::default(),
            },
            EventV2::ModelObserved {
                mol_id: mol.clone(),
                worker_id: Some(worker.clone()),
                adapter_name: "claude".into(),
                model: "model-a".into(),
                observed_source: ModelObservationSource::ClaudeStreamJson,
                provenance: None,
                observed_at: selected_at,
            },
            EventV2::ModelEvidenceAssessed {
                mol_id: mol.clone(),
                worker_id: worker,
                adapter_name: "claude".into(),
                policy_version: 1,
                observation_basis: ModelEvidenceGrammar::Claude,
                generation: ModelEvidenceGeneration(1),
                assessment: assess_claude_model_evidence(
                    b"{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n{\"type\":\"assistant\",\"message\":{}}\n",
                    true,
                ),
                captured_at: selected_at,
            },
        ];
        for event in events {
            emit_one(&log, event, None).unwrap();
        }
        let attribution = cosmon_state::ops::realized_attribution(&state_dir, &mol).unwrap();
        let compact = attribution.compact_cell();
        assert!(compact.starts_with("!claude"), "{compact}");
        assert_eq!(compact.contains("/model-a"), pinned);

        let text = command(tmp.path())
            .args([
                "--config",
                state_dir.to_str().unwrap(),
                "observe",
                mol.as_str(),
            ])
            .output()
            .unwrap();
        assert!(
            text.status.success(),
            "{}",
            String::from_utf8_lossy(&text.stderr)
        );
        let rendered = String::from_utf8_lossy(&text.stdout);
        assert!(
            rendered.contains("last observed; coverage degraded"),
            "{rendered}"
        );
        let json = command(tmp.path())
            .args([
                "--json",
                "--config",
                state_dir.to_str().unwrap(),
                "observe",
                mol.as_str(),
            ])
            .output()
            .unwrap();
        assert!(
            json.status.success(),
            "{}",
            String::from_utf8_lossy(&json.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(value["realized"]["observed"][0], "model-a");
        assert_eq!(
            value["model_evidence"]["assessed"]["assessment"]["coverage"]["degraded"][0],
            "missing_assistant_model"
        );
        assert_eq!(
            value["realized_disposition"],
            "last observed; coverage degraded"
        );
        assert_eq!(value["model"].is_null(), !pinned);
    }
}
