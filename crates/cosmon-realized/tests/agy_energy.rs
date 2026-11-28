// SPDX-License-Identifier: AGPL-3.0-only

//! Antigravity receipt reads through the same worker collector as Peek.

use std::collections::HashMap;
use std::fs;

use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, MoleculeId, WorkerId};
use cosmon_core::usage::{TokenCount, UnavailableReason};
use cosmon_core::worker::{WorkerRole, WorkerStatus};
use cosmon_filestore::FileStore;
use cosmon_realized::energy_probe::{
    load_worker_energy_with_adapters, read_agy_receipt, WorkerEnergy,
};
use cosmon_state::{Fleet, WorkerData};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Receipt {
    state: tempfile::TempDir,
    id: MoleculeId,
    worker_id: WorkerId,
}

impl Receipt {
    fn new() -> TestResult<Self> {
        Ok(Self {
            state: tempfile::tempdir()?,
            id: MoleculeId::new("task-20261126-a901")?,
            worker_id: WorkerId::new("worker-agy-test")?,
        })
    }

    fn write(&self, receipt: &[u8]) -> std::io::Result<()> {
        let dir = FileStore::new(self.state.path()).molecule_dir(&self.id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("agy-stream.jsonl"), receipt)
    }

    fn observe(
        &self,
        adapter: &str,
        selected_role: WorkerRole,
    ) -> TestResult<Option<WorkerEnergy>> {
        let mut worker = WorkerData::new(
            self.worker_id.clone(),
            AgentId::new("receipt-test")?,
            AgentRole::Implementation,
            Clearance::Execute,
            WorkerStatus::Stopped,
        )
        .with_worker_role(selected_role);
        worker.current_molecule = Some(self.id.clone());
        let mut fleet = Fleet::new();
        fleet.workers.insert(self.worker_id.clone(), worker);
        let adapters = HashMap::from([(self.id.clone(), adapter.to_owned())]);
        Ok(
            load_worker_energy_with_adapters(self.state.path(), &[], &fleet, &adapters)
                .remove(&self.worker_id),
        )
    }
}

#[test]
fn the_worker_collector_uses_receipt_without_a_live_pane() -> TestResult<()> {
    let fixture = Receipt::new()?;
    let result = include_bytes!("../../cosmon-cli/tests/fixtures/agy/quota.jsonl");
    fixture.write(result)?;
    let receipt = read_agy_receipt(fixture.state.path(), &fixture.id);
    assert_eq!(receipt.usage.unwrap().cache_read, Some(369_075));
    let energy = fixture.observe("agy", WorkerRole::Cognition)?.unwrap();
    assert_eq!(energy.token_tuple(), (528_109, 369_075, 8_486, 4_698));
    let receipt = energy.agy_receipt.unwrap();
    assert_eq!(receipt.usage.unwrap().input, Some(159_034));
    assert_eq!(
        receipt.quota_stop.unwrap().reset_delay.as_deref(),
        Some("19m45s")
    );
    assert!(fixture.observe("agy", WorkerRole::Runtime)?.is_none());
    assert!(fixture.observe("codex", WorkerRole::Cognition)?.is_none());

    let mut torn = result.to_vec();
    torn.extend_from_slice(b"{\"event\":\"result\"");
    fixture.write(&torn)?;
    let reread = fixture.observe("agy", WorkerRole::Cognition)?.unwrap();
    assert_eq!(
        energy.usage.unwrap().observation_id,
        reread.usage.unwrap().observation_id
    );
    Ok(())
}

#[test]
fn unknown_or_inconsistent_counts_do_not_invent_a_canonical_total() -> TestResult<()> {
    let fixture = Receipt::new()?;
    let missing = fixture.observe("agy", WorkerRole::Cognition)?.unwrap();
    assert!(missing.usage.is_none());
    assert!(missing.agy_receipt.unwrap().usage.is_none());

    fixture.write(br#"{"event":"result","result":{"usage":{"cache_read_tokens":15,"output_tokens":3,"thinking_tokens":8}}}"#)?;
    let partial = fixture.observe("agy", WorkerRole::Cognition)?.unwrap();
    let record = partial.usage.unwrap();
    assert_eq!(record.tokens.input_tokens.measured(), None);
    assert_eq!(record.tokens.cached_input_tokens.measured(), Some(15));
    assert_eq!(record.tokens.output_tokens.measured(), Some(3));
    assert_eq!(
        record.tokens.reasoning_output_tokens,
        TokenCount::Unavailable {
            reason: UnavailableReason::MalformedSource
        }
    );
    assert_eq!(
        partial.agy_receipt.unwrap().usage.unwrap().thinking,
        Some(8)
    );
    fixture.write(
        br#"{"event":"result","result":{"usage":{"output_tokens":3,"thinking_tokens":3}}}"#,
    )?;
    let all_reasoning = fixture.observe("agy", WorkerRole::Cognition)?.unwrap();
    assert_eq!(
        all_reasoning
            .usage
            .unwrap()
            .tokens
            .reasoning_output_tokens
            .measured(),
        Some(3)
    );
    Ok(())
}

#[test]
fn a_new_init_starts_a_new_cumulative_history_in_an_appended_receipt() -> TestResult<()> {
    let fixture = Receipt::new()?;
    let first_session = include_bytes!("../../cosmon-cli/tests/fixtures/agy/quota.jsonl");
    fixture.write(first_session)?;
    let first = fixture
        .observe("agy", WorkerRole::Cognition)?
        .unwrap()
        .usage
        .unwrap();

    let mut receipt = first_session.to_vec();
    receipt.extend_from_slice(br#"{"event":"result","result":{"status":"SUCCESS","usage":{"input_tokens":10,"cache_read_tokens":0,"output_tokens":5,"thinking_tokens":0}}}"#);
    receipt.push(b'\n');
    fixture.write(&receipt)?;
    let same_session = fixture
        .observe("agy", WorkerRole::Cognition)?
        .unwrap()
        .usage
        .unwrap();
    assert_eq!(first.subject.history, same_session.subject.history);

    receipt.extend_from_slice(first_session);
    fixture.write(&receipt)?;
    let restarted = fixture
        .observe("agy", WorkerRole::Cognition)?
        .unwrap()
        .usage
        .unwrap();
    assert_ne!(first.subject.history, restarted.subject.history);
    Ok(())
}
