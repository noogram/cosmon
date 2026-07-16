// SPDX-License-Identifier: AGPL-3.0-only

//! File-based `EnergyTracker` adapter.
//!
//! - Records are appended to a JSONL file (one JSON object per line).
//! - Budget is loaded from a JSON config file.
//! - Reports aggregate from the JSONL log filtered by period.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use cosmon_core::energy::{BudgetPeriod, EnergyBudget, EnergyRecord, EnergyReport, TokenCount};
use cosmon_core::error::CosmonError;
use cosmon_core::id::WorkerId;

use crate::EnergyTracker;

/// File-based energy tracker.
///
/// - Appends `EnergyRecord` entries to `{ops_root}/log/energy.jsonl`
/// - Reads `EnergyBudget` from `{ops_root}/config/energy-budget.json`
pub struct FileEnergyTracker {
    log_path: PathBuf,
    budget_path: PathBuf,
}

impl FileEnergyTracker {
    /// Create a new tracker rooted at `ops_root`.
    ///
    /// The directory structure is:
    /// ```text
    /// ops_root/
    ///   log/energy.jsonl
    ///   config/energy-budget.json
    /// ```
    #[must_use]
    pub fn new(ops_root: &Path) -> Self {
        Self {
            log_path: ops_root.join("log/energy.jsonl"),
            budget_path: ops_root.join("config/energy-budget.json"),
        }
    }

    /// Read all records from the JSONL log, skipping malformed lines.
    fn read_records(&self) -> Result<Vec<EnergyRecord>, CosmonError> {
        if !self.log_path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.log_path)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<EnergyRecord>(trimmed) {
                records.push(record);
            }
        }
        Ok(records)
    }
}

impl EnergyTracker for FileEnergyTracker {
    fn record(&self, record: &EnergyRecord) -> Result<(), CosmonError> {
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        let json = serde_json::to_string(record)?;
        writeln!(file, "{json}")?;
        Ok(())
    }

    fn budget(&self) -> Result<EnergyBudget, CosmonError> {
        let data = fs::read_to_string(&self.budget_path).map_err(|e| CosmonError::StateStore {
            reason: format!(
                "failed to read energy budget from {}: {e}",
                self.budget_path.display()
            ),
        })?;
        serde_json::from_str(&data).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to parse energy budget: {e}"),
        })
    }

    fn report(&self, period: &BudgetPeriod) -> Result<EnergyReport, CosmonError> {
        let records = self.read_records()?;

        // Filter records by period
        let filtered: Vec<&EnergyRecord> = records
            .iter()
            .filter(|r| match period {
                BudgetPeriod::PerMolecule(mol_id) => &r.molecule == mol_id,
                // Weekly/Monthly: include all records (time filtering is a future concern)
                BudgetPeriod::Weekly | BudgetPeriod::Monthly => true,
            })
            .collect();

        // Aggregate by worker
        let mut worker_map: HashMap<WorkerId, TokenCount> = HashMap::new();
        let mut mol_map: HashMap<cosmon_core::id::MoleculeId, TokenCount> = HashMap::new();
        let mut total = TokenCount::new(0);

        for r in &filtered {
            let tokens = r.total_tokens();
            *worker_map.entry(r.worker.clone()).or_default() =
                *worker_map.get(&r.worker).unwrap_or(&TokenCount::new(0)) + tokens;
            *mol_map.entry(r.molecule.clone()).or_default() =
                *mol_map.get(&r.molecule).unwrap_or(&TokenCount::new(0)) + tokens;
            total = total + tokens;
        }

        let by_worker: Vec<_> = worker_map.into_iter().collect();
        let by_molecule: Vec<_> = mol_map.into_iter().collect();

        Ok(EnergyReport {
            by_worker,
            by_molecule,
            entropy_tax: TokenCount::new(0),
            productive_tokens: total,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cosmon_core::energy::{TokenCost, TokenCount};
    use cosmon_core::id::{MoleculeId, StepId, WorkerId};
    use tempfile::TempDir;

    fn sample_record(worker: &str, molecule: &str, input: u64, output: u64) -> EnergyRecord {
        EnergyRecord {
            timestamp: Utc::now(),
            worker: WorkerId::new(worker).unwrap(),
            molecule: MoleculeId::new(molecule).unwrap(),
            step: StepId::new("step-1").unwrap(),
            model: "claude-opus-4-6".to_owned(),
            input_tokens: TokenCount::new(input),
            output_tokens: TokenCount::new(output),
            cost: TokenCost::new(0.006),
        }
    }

    fn sample_budget() -> EnergyBudget {
        EnergyBudget::new(TokenCount::new(100_000), BudgetPeriod::Weekly, 0.8)
    }

    fn write_budget(dir: &Path, budget: &EnergyBudget) {
        let config_dir = dir.join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let json = serde_json::to_string_pretty(budget).unwrap();
        fs::write(config_dir.join("energy-budget.json"), json).unwrap();
    }

    #[test]
    fn test_record_appends_jsonl() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        let r1 = sample_record("topaz", "cs-20260401-aaaa", 1000, 500);
        let r2 = sample_record("quartz", "cs-20260401-bbbb", 2000, 800);

        tracker.record(&r1).unwrap();
        tracker.record(&r2).unwrap();

        // Verify JSONL: two lines, each valid JSON
        let content = fs::read_to_string(tmp.path().join("log/energy.jsonl")).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let parsed1: EnergyRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed1.worker, WorkerId::new("topaz").unwrap());

        let parsed2: EnergyRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed2.worker, WorkerId::new("quartz").unwrap());
    }

    #[test]
    fn test_budget_loads_from_config() {
        let tmp = TempDir::new().unwrap();
        let expected = sample_budget();
        write_budget(tmp.path(), &expected);

        let tracker = FileEnergyTracker::new(tmp.path());
        let loaded = tracker.budget().unwrap();

        assert_eq!(loaded.total, expected.total);
        assert_eq!(loaded.period, expected.period);
        assert!((loaded.alert_threshold - expected.alert_threshold).abs() < f64::EPSILON);
    }

    #[test]
    fn test_report_aggregates_by_worker() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        // Topaz: 2 records, Quartz: 1 record
        tracker
            .record(&sample_record("topaz", "cs-20260401-aaaa", 1000, 500))
            .unwrap();
        tracker
            .record(&sample_record("topaz", "cs-20260401-bbbb", 2000, 800))
            .unwrap();
        tracker
            .record(&sample_record("quartz", "cs-20260401-aaaa", 500, 200))
            .unwrap();

        let report = tracker.report(&BudgetPeriod::Weekly).unwrap();

        // Find workers in aggregated report
        let topaz_id = WorkerId::new("topaz").unwrap();
        let quartz_id = WorkerId::new("quartz").unwrap();

        let topaz_tokens = report
            .by_worker
            .iter()
            .find(|(w, _)| w == &topaz_id)
            .map(|(_, t)| t.get())
            .unwrap();
        let quartz_tokens = report
            .by_worker
            .iter()
            .find(|(w, _)| w == &quartz_id)
            .map(|(_, t)| t.get())
            .unwrap();

        // topaz: (1000+500) + (2000+800) = 4300
        assert_eq!(topaz_tokens, 4300);
        // quartz: 500+200 = 700
        assert_eq!(quartz_tokens, 700);
    }

    #[test]
    fn test_free_energy_ratio() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        tracker
            .record(&sample_record("topaz", "cs-20260401-aaaa", 600, 400))
            .unwrap();

        let report = tracker.report(&BudgetPeriod::Weekly).unwrap();

        // Total = 1000, productive = 1000 (entropy_tax = 0 in this impl)
        assert_eq!(report.total_tokens().get(), 1000);
        assert!((report.free_energy_ratio() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_report_filters_by_molecule() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        tracker
            .record(&sample_record("topaz", "cs-20260401-aaaa", 1000, 500))
            .unwrap();
        tracker
            .record(&sample_record("topaz", "cs-20260401-bbbb", 2000, 800))
            .unwrap();

        let mol_id = MoleculeId::new("cs-20260401-aaaa").unwrap();
        let report = tracker.report(&BudgetPeriod::PerMolecule(mol_id)).unwrap();

        // Only the first record (1000+500=1500)
        assert_eq!(report.total_tokens().get(), 1500);
    }

    #[test]
    fn test_report_empty_log() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        let report = tracker.report(&BudgetPeriod::Weekly).unwrap();
        assert_eq!(report.total_tokens().get(), 0);
        assert!(report.by_worker.is_empty());
    }

    #[test]
    fn test_budget_missing_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tracker = FileEnergyTracker::new(tmp.path());

        let result = tracker.budget();
        assert!(result.is_err());
    }
}
