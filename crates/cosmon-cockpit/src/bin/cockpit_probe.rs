// SPDX-License-Identifier: AGPL-3.0-only

//! 72-hour evidence-gathering probe for cockpit observability.
//!
//! Walks `.cosmon/state/`, reads the energy log, and emits one NDJSON line
//! per interval with: regime population, total token spend, worker liveness
//! histogram, and nucleation count. No UI — pure measurement.
//!
//! ```text
//! cargo run -p cosmon-cockpit --bin cockpit-probe -- \
//!     --state .cosmon/state --out probe.ndjson --interval 60s
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use serde::Serialize;

use cosmon_core::energy::EnergyRecord;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};

/// Cockpit probe — silent NDJSON telemetry emitter.
#[derive(Parser)]
#[command(name = "cockpit-probe")]
struct Cli {
    /// Path to `.cosmon/state/` directory.
    #[arg(long)]
    state: PathBuf,

    /// Output NDJSON file path.
    #[arg(long)]
    out: PathBuf,

    /// Sampling interval (e.g. `60s`, `5m`).
    #[arg(long, value_parser = parse_duration)]
    interval: Duration,
}

/// A single probe sample emitted as one NDJSON line.
#[derive(Debug, Serialize)]
struct ProbeSample {
    /// ISO-8601 timestamp of this sample.
    timestamp: String,
    /// Molecule counts by status (regime population).
    regime: RegimePopulation,
    /// Total token spend aggregated from the energy log.
    energy: EnergySummary,
    /// Worker liveness histogram: healthy / zombie / mismatch / unknown.
    liveness: LivenessHistogram,
    /// Total number of molecules ever nucleated (proxy: total molecule count).
    nucleation_count: usize,
}

/// Molecule counts per lifecycle status.
#[derive(Debug, Default, Serialize)]
struct RegimePopulation {
    pending: usize,
    queued: usize,
    running: usize,
    frozen: usize,
    starved: usize,
    completed: usize,
    collapsed: usize,
    total: usize,
}

/// Aggregated token spend from `log/energy.jsonl`.
#[derive(Debug, Default, Serialize)]
struct EnergySummary {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
}

/// Liveness histogram across all molecules.
#[derive(Debug, Default, Serialize)]
struct LivenessHistogram {
    healthy: usize,
    zombie: usize,
    mismatch: usize,
    unknown: usize,
}

fn main() {
    let cli = Cli::parse();
    let running = Arc::new(AtomicBool::new(true));
    let r = Arc::clone(&running);
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
        .expect("failed to set Ctrl-C handler");

    while running.load(Ordering::SeqCst) {
        let sample = collect_sample(&cli.state);
        if let Err(e) = append_sample(&cli.out, &sample) {
            eprintln!("cockpit-probe: write error: {e}");
        }

        // Sleep in short chunks so Ctrl-C is responsive.
        let mut remaining = cli.interval;
        while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
            let chunk = remaining.min(Duration::from_secs(1));
            std::thread::sleep(chunk);
            remaining = remaining.saturating_sub(chunk);
        }
    }
}

/// Collect one snapshot from the state directory.
fn collect_sample(state_dir: &Path) -> ProbeSample {
    let regime = collect_regime(state_dir);
    let energy = collect_energy(state_dir);
    let liveness = collect_liveness(state_dir);
    let nucleation_count = regime.total;

    ProbeSample {
        timestamp: Utc::now().to_rfc3339(),
        regime,
        energy,
        liveness,
        nucleation_count,
    }
}

/// Count molecules by status.
fn collect_regime(state_dir: &Path) -> RegimePopulation {
    let store = FileStore::new(state_dir);
    let Ok(mols) = store.list_molecules(&MoleculeFilter::default()) else {
        return RegimePopulation::default();
    };

    let mut pop = RegimePopulation {
        total: mols.len(),
        ..Default::default()
    };
    for m in &mols {
        match m.status {
            MoleculeStatus::Pending => pop.pending += 1,
            MoleculeStatus::Queued => pop.queued += 1,
            MoleculeStatus::Running => pop.running += 1,
            MoleculeStatus::Frozen => pop.frozen += 1,
            MoleculeStatus::Starved => pop.starved += 1,
            MoleculeStatus::Completed => pop.completed += 1,
            MoleculeStatus::Collapsed => pop.collapsed += 1,
            // `MoleculeStatus` is non_exhaustive (ADR-062 mitigation);
            // any unknown future variant lands in the catch-all to keep
            // the cockpit probe future-proof against minor bumps.
            _ => {}
        }
    }
    pop
}

/// Sum total token spend from the energy JSONL log.
fn collect_energy(state_dir: &Path) -> EnergySummary {
    let log_path = state_dir.join("log/energy.jsonl");
    if !log_path.exists() {
        return EnergySummary::default();
    }
    let Ok(file) = File::open(&log_path) else {
        return EnergySummary::default();
    };
    let reader = BufReader::new(file);
    let mut summary = EnergySummary::default();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<EnergyRecord>(trimmed) else {
            continue;
        };
        summary.input_tokens = summary
            .input_tokens
            .saturating_add(record.input_tokens.get());
        summary.output_tokens = summary
            .output_tokens
            .saturating_add(record.output_tokens.get());
        summary.cost_usd += record.cost.get();
    }
    summary.total_tokens = summary.input_tokens.saturating_add(summary.output_tokens);
    summary
}

/// Build a liveness histogram by cross-referencing molecule status with
/// worker live state from cognitive status files.
fn collect_liveness(state_dir: &Path) -> LivenessHistogram {
    let store = FileStore::new(state_dir);
    let Ok(mols) = store.list_molecules(&MoleculeFilter::default()) else {
        return LivenessHistogram::default();
    };

    // Read cognitive status files: {state_dir}/cognitive/{worker_id}.json
    let cognitive_dir = state_dir.join("cognitive");
    let cognitive_map = load_cognitive_statuses(&cognitive_dir);

    let mut hist = LivenessHistogram::default();
    for m in &mols {
        let worker_live = m
            .assigned_worker
            .as_ref()
            .and_then(|w| cognitive_map.get(w.as_str()));
        let liveness = cosmon_cockpit::view::compute_liveness(
            &m.status.to_string(),
            worker_live.map(String::as_str),
        );
        match liveness {
            cosmon_cockpit::view::Liveness::Healthy => hist.healthy += 1,
            cosmon_cockpit::view::Liveness::Zombie => hist.zombie += 1,
            cosmon_cockpit::view::Liveness::Mismatch => hist.mismatch += 1,
            cosmon_cockpit::view::Liveness::Unknown => hist.unknown += 1,
        }
    }
    hist
}

/// Load cognitive status strings from `{cognitive_dir}/{worker}.json` files.
/// Returns `worker_id` → live status string.
fn load_cognitive_statuses(cognitive_dir: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(cognitive_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(status) = val.get("status").and_then(|s| s.as_str()) {
                            map.insert(stem.to_owned(), status.to_owned());
                        }
                    }
                }
            }
        }
    }
    map
}

/// Append one NDJSON line to the output file.
fn append_sample(out: &Path, sample: &ProbeSample) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(out)?;
    let line = serde_json::to_string(sample).map_err(|e| std::io::Error::other(e.to_string()))?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Parse a human-friendly duration string (e.g. `60s`, `5m`, `1h`).
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_owned());
    }
    let (num, suffix) = if let Some(n) = s.strip_suffix('s') {
        (n, "s")
    } else if let Some(n) = s.strip_suffix('m') {
        (n, "m")
    } else if let Some(n) = s.strip_suffix('h') {
        (n, "h")
    } else {
        (s, "s") // default to seconds
    };
    let n: u64 = num
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    match suffix {
        "s" => Ok(Duration::from_secs(n)),
        "m" => Ok(Duration::from_secs(n * 60)),
        "h" => Ok(Duration::from_secs(n * 3600)),
        _ => Err(format!("unknown suffix: {suffix}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn test_parse_duration_bare_number() {
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn test_collect_regime_empty_state() {
        let tmp = TempDir::new().unwrap();
        let regime = collect_regime(tmp.path());
        assert_eq!(regime.total, 0);
    }

    #[test]
    fn test_collect_energy_no_log() {
        let tmp = TempDir::new().unwrap();
        let energy = collect_energy(tmp.path());
        assert_eq!(energy.total_tokens, 0);
        assert!((energy.cost_usd - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_append_sample_creates_file() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("probe.ndjson");
        let sample = ProbeSample {
            timestamp: "2026-04-10T12:00:00Z".to_owned(),
            regime: RegimePopulation::default(),
            energy: EnergySummary::default(),
            liveness: LivenessHistogram::default(),
            nucleation_count: 0,
        };
        append_sample(&out, &sample).unwrap();
        let content = fs::read_to_string(&out).unwrap();
        assert!(content.contains("\"timestamp\""));
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_collect_liveness_no_cognitive_dir() {
        let tmp = TempDir::new().unwrap();
        let hist = collect_liveness(tmp.path());
        assert_eq!(hist.healthy, 0);
        assert_eq!(hist.zombie, 0);
    }
}
