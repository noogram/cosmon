// SPDX-License-Identifier: AGPL-3.0-only

//! Admission adapters for `cs tackle` (issue #214, ADR-190 D2).
//!
//! The decision is [`cosmon_core::spawn_admission::admit`]. This module gathers the
//! facts behind two ports (the molecule store plus a transport liveness check,
//! and a free-space probe) and turns a refusal into the error `cs tackle`
//! returns. It runs before the dispatch claim, so a refused spawn leaves no
//! worktree, branch or session.

use std::path::Path;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::spawn_admission::{
    admit, counts_as_active, Admission, AdmissionConfig, AdmissionFacts,
};
use cosmon_core::transport::TransportBackend;
use cosmon_state::{MoleculeFilter, StateStore};

/// Port: free bytes on the volume that holds a path.
pub(crate) trait FreeSpaceProbe {
    /// Free bytes available to this user on the volume holding `path`.
    ///
    /// # Errors
    /// Returns the reason when the figure cannot be established.
    fn free_bytes(&self, path: &Path) -> Result<u64, String>;
}

/// The real probe: `df -Pk` on the nearest existing ancestor of the path.
pub(crate) struct DfProbe;

impl FreeSpaceProbe for DfProbe {
    fn free_bytes(&self, path: &Path) -> Result<u64, String> {
        let existing = path
            .ancestors()
            .find(|p| p.exists())
            .ok_or_else(|| format!("no existing ancestor of {}", path.display()))?;
        let out = std::process::Command::new("df")
            .arg("-Pk")
            .arg(existing)
            .output()
            .map_err(|e| format!("cannot run df: {e}"))?;
        if !out.status.success() {
            return Err(format!("df exited with {}", out.status));
        }
        parse_df_available_kib(&String::from_utf8_lossy(&out.stdout))
            .map(|kib| kib.saturating_mul(1024))
            .ok_or_else(|| "unparseable df output".to_owned())
    }
}

/// The "Available" column (KiB) of POSIX `df -P` output, second line.
fn parse_df_available_kib(output: &str) -> Option<u64> {
    output
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

/// Count active molecules by [`counts_as_active`], never counting `exclude`.
fn count_active(
    store: &dyn StateStore,
    backend: &dyn TransportBackend,
    exclude: &MoleculeId,
) -> Result<u32, String> {
    let running = store
        .list_molecules(&MoleculeFilter {
            status: Some(MoleculeStatus::Running),
            ..MoleculeFilter::default()
        })
        .map_err(|e| format!("cannot list molecules: {e}"))?;
    let mut active = 0_u32;
    for mol in running.iter().filter(|m| &m.id != exclude) {
        let alive = mol
            .process
            .as_ref()
            .is_some_and(|p| backend.is_alive(&p.worker_id).unwrap_or(false));
        if counts_as_active(mol.status, alive) {
            active += 1;
        }
    }
    Ok(active)
}

/// Gather facts and refuse the spawn when a configured bound is exceeded.
///
/// Unset config does nothing and costs nothing. A fact that cannot be
/// gathered is reported on `notice` and treated as unknown, which admits.
///
/// # Errors
/// Returns the refusal, naming the bound, the observed value and the remedy.
pub(crate) fn enforce(
    config: &AdmissionConfig,
    store: &dyn StateStore,
    backend: &dyn TransportBackend,
    probe: &dyn FreeSpaceProbe,
    mol_id: &MoleculeId,
    worktrees_root: &Path,
    notice: &mut dyn FnMut(&str),
) -> anyhow::Result<()> {
    if !config.is_enabled() {
        return Ok(());
    }
    let mut facts = AdmissionFacts::default();
    if config.max_active.is_some() {
        match count_active(store, backend, mol_id) {
            Ok(n) => facts.active = Some(n),
            Err(e) => notice(&format!(
                "admission: active-molecule count unavailable ({e}); admitting"
            )),
        }
    }
    if config.min_free_bytes.is_some() {
        match probe.free_bytes(worktrees_root) {
            Ok(n) => facts.free_bytes = Some(n),
            Err(e) => notice(&format!(
                "admission: free-space probe failed ({e}); admitting"
            )),
        }
    }
    match admit(config, &facts) {
        Admission::Admitted => Ok(()),
        Admission::RefusedMaxActive { max_active, active } => Err(anyhow::anyhow!(
            "cs tackle: admission refused for {mol_id}: {active} active molecules, \
             max_active is {max_active}. Wait for one to finish, or raise \
             [admission].max_active in .cosmon/config.toml."
        )),
        Admission::RefusedMinFree {
            min_free_bytes,
            free_bytes,
        } => Err(anyhow::anyhow!(
            "cs tackle: admission refused for {mol_id}: {free_bytes} bytes free on the \
             volume holding {}, min_free_bytes is {min_free_bytes}. Free space, or lower \
             [admission].min_free_bytes in .cosmon/config.toml.",
            worktrees_root.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, WorkerId};
    use cosmon_core::process::MoleculeProcess;
    use cosmon_filestore::FileStore;
    use cosmon_state::MoleculeData;
    use cosmon_transport::mock::MockBackend;
    use std::collections::HashMap;

    fn sample_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            harvest_reason: None,
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            base_branch: None,
            protected_paths: Vec::new(),
            pending_step: None,
            merged_at: None,
            non_integration: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            collapse_receipt: None,
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    /// Fake free-space port.
    struct FakeProbe(Result<u64, String>);

    impl FreeSpaceProbe for FakeProbe {
        fn free_bytes(&self, _path: &Path) -> Result<u64, String> {
            self.0.clone()
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: FileStore,
        backend: MockBackend,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let store = FileStore::new(tmp.path());
            store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
            Self {
                _tmp: tmp,
                store,
                backend: MockBackend::new(),
            }
        }

        /// Save a molecule with a recorded worker; `alive` decides whether the
        /// transport knows the session.
        fn add(&self, id: &str, status: MoleculeStatus, alive: bool) {
            let mut mol = sample_mol(id, status);
            let wid = WorkerId::new(id).unwrap();
            mol.process = Some(MoleculeProcess::new(wid, id));
            if alive {
                let agent = cosmon_core::transport::AgentDefinition {
                    id: cosmon_core::id::AgentId::new(id).unwrap(),
                    role: cosmon_core::agent::AgentRole::Implementation,
                    command: "true".to_owned(),
                    args: vec![],
                    cwd: None,
                };
                self.backend
                    .spawn(&agent, &cosmon_core::transport::RuntimeConfig::default())
                    .unwrap();
            }
            self.store.save_molecule(&mol.id.clone(), &mol).unwrap();
        }

        fn run(
            &self,
            config: &AdmissionConfig,
            probe: &FakeProbe,
            notices: &mut Vec<String>,
        ) -> anyhow::Result<()> {
            enforce(
                config,
                &self.store,
                &self.backend,
                probe,
                &MoleculeId::new("t-20261117-0009").unwrap(),
                Path::new("/wt"),
                &mut |m| notices.push(m.to_owned()),
            )
        }
    }

    fn cap(n: u32) -> AdmissionConfig {
        AdmissionConfig {
            max_active: Some(n),
            min_free_bytes: None,
        }
    }

    #[test]
    fn cap_two_with_two_live_running_molecules_refuses_the_third() {
        let fx = Fixture::new();
        fx.add("t-20261117-0001", MoleculeStatus::Running, true);
        fx.add("t-20261117-0002", MoleculeStatus::Running, true);
        let err = fx
            .run(&cap(2), &FakeProbe(Ok(u64::MAX)), &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 active molecules"), "{err}");
        assert!(err.contains("max_active is 2"), "{err}");
    }

    #[test]
    fn completed_unharvested_awaiting_and_dead_session_molecules_are_not_counted() {
        let fx = Fixture::new();
        fx.add("t-20261117-0001", MoleculeStatus::Running, true);
        fx.add("t-20261117-0003", MoleculeStatus::Completed, true);
        fx.add("t-20261117-0004", MoleculeStatus::Frozen, true);
        // Running with a dead session: what an awaiting-human worker that
        // exited, or a crashed one, looks like to the count.
        fx.add("t-20261117-0005", MoleculeStatus::Running, false);
        fx.run(&cap(2), &FakeProbe(Ok(u64::MAX)), &mut Vec::new())
            .unwrap();
    }

    #[test]
    fn the_molecule_being_tackled_is_not_counted_against_itself() {
        let fx = Fixture::new();
        fx.add("t-20261117-0001", MoleculeStatus::Running, true);
        fx.add("t-20261117-0009", MoleculeStatus::Running, true);
        fx.run(&cap(2), &FakeProbe(Ok(u64::MAX)), &mut Vec::new())
            .unwrap();
    }

    #[test]
    fn unset_config_admits_whatever_the_count_and_never_probes() {
        let fx = Fixture::new();
        for id in ["t-20261117-000a", "t-20261117-000b", "t-20261117-000c"] {
            fx.add(id, MoleculeStatus::Running, true);
        }
        let mut notices = Vec::new();
        fx.run(
            &AdmissionConfig::default(),
            &FakeProbe(Err("must not be called".into())),
            &mut notices,
        )
        .unwrap();
        assert!(notices.is_empty());
    }

    #[test]
    fn a_floor_above_the_free_bytes_refuses_naming_the_observed_value() {
        let fx = Fixture::new();
        let config = AdmissionConfig {
            max_active: None,
            min_free_bytes: Some(10_000),
        };
        let err = fx
            .run(&config, &FakeProbe(Ok(1234)), &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("1234 bytes free"), "{err}");
        assert!(err.contains("min_free_bytes is 10000"), "{err}");
    }

    #[test]
    fn a_failed_probe_admits_and_says_so() {
        let fx = Fixture::new();
        let config = AdmissionConfig {
            max_active: None,
            min_free_bytes: Some(10_000),
        };
        let mut notices = Vec::new();
        fx.run(&config, &FakeProbe(Err("df exploded".into())), &mut notices)
            .unwrap();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("df exploded"), "{notices:?}");
        assert!(notices[0].contains("admitting"), "{notices:?}");
    }

    #[test]
    fn df_output_is_parsed_to_the_available_column() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/disk3s1 488245288 400000000 88245288 82% /\n";
        assert_eq!(parse_df_available_kib(out), Some(88_245_288));
        assert_eq!(parse_df_available_kib("garbage"), None);
    }
}
